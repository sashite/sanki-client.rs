// SPDX-License-Identifier: Apache-2.0
//! An in-process NIP-01 relay for tests (`testing` feature; ADR-0045 §1
//! *testing*) — a client connects to it like to any relay, while the test
//! holds the OTHER side of the wire: it can inject already-signed events
//! straight into the store (acting as the opponent without a second client),
//! observe EVERY frame the client publishes (`received` — even the swallowed
//! ones), and inject faults:
//!
//! - `swallow_plies_from` acknowledges a Ply with `OK true` but neither
//!   stores nor delivers it — the "relay accepted it, nobody ever saw it" gap
//!   behind the fleet's double-publish incident;
//! - `set_window` enforces the strict `created_at` window of the reference
//!   relay on the covered kinds, with its exact rejection wordings, so that
//!   a client's stamping and skew correction are tested against the contract
//!   the production relay implements (`sashite-nostr-relay-timing-policy`);
//! - `set_min_pow` requires the NIP-13 `nonce` tag on the kinds that
//!   prescribe it, mined to a minimum.
//!
//! Filter support is the subset a client uses: `ids`, `authors`, `kinds`,
//! `#e` / `#p` (any single-letter tag), `since`, `until`. `limit` is ignored
//! (a test stores dozens of events, never thousands).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::Mutex;

use crate::relay::{Limitation, RelayInfo};

/// The kinds the reference relay's strict window covers (its
/// `SASHITE_TIMED_KINDS` default): the founding kinds, the Ply, the Conclusion.
pub const TIMED_KINDS: [u64; 6] = [3418, 3419, 3420, 3422, 3423, 3425];

/// The kinds that prescribe a NIP-13 `nonce` tag (the reference relay's
/// `POW_KINDS`).
pub const POW_KINDS: [u64; 6] = [3417, 3418, 3420, 3423, 3425, 30422];

/// The strict `created_at` window, as deltas in seconds around the relay's
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Seconds into the past still accepted.
    pub past: u64,
    /// Seconds into the future still accepted.
    pub future: u64,
}

impl Window {
    /// The reference relay's production window: 1 s past, 5 s future.
    pub const REFERENCE: Self = Self { past: 1, future: 5 };
}

/// One EVENT frame as the relay received it (stored, swallowed or rejected
/// alike).
#[derive(Debug, Clone)]
pub struct ReceivedEvent {
    /// The signer.
    pub pubkey: String,
    /// The kind.
    pub kind: u64,
    /// The content.
    pub content: String,
    /// The first `step` tag value, when present (Ply slot ordinal).
    pub step: Option<String>,
    /// Wall-clock receipt instant — grace-delay assertions read this.
    pub at: Instant,
}

struct Subscription {
    sub_id: String,
    filters: Vec<Value>,
    outbound: UnboundedSender<String>,
}

/// The relay's shared state.
#[derive(Default)]
pub struct RelayState {
    stored: Mutex<Vec<Value>>,
    received: Mutex<Vec<ReceivedEvent>>,
    swallow_plies_from: Mutex<Option<String>>,
    window: Mutex<Option<Window>>,
    /// A fixed offset of the relay's clock from the host's (seconds), to
    /// simulate skew.
    skew: Mutex<i64>,
    min_pow: Mutex<u8>,
    subscriptions: Mutex<Vec<Subscription>>,
}

/// A running relay: its URL and its state.
pub struct MiniRelay {
    /// The `ws://127.0.0.1:<port>` URL a client connects to.
    pub url: String,
    /// The shared state.
    pub state: Arc<RelayState>,
}

const PLY_KIND: u64 = 3423;

impl MiniRelay {
    /// Starts a relay on a free loopback port, with no window and no proof
    /// of work required.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let state = Arc::new(RelayState::default());
        let accept_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(handle_connection(stream, Arc::clone(&accept_state)));
            }
        });
        Self {
            url: format!("ws://127.0.0.1:{port}"),
            state,
        }
    }

    /// Acknowledge-but-drop every Ply (kind 3423) from `pubkey_hex` — or stop.
    pub async fn swallow_plies_from(&self, pubkey_hex: Option<String>) {
        *self.state.swallow_plies_from.lock().await = pubkey_hex;
    }

    /// Enforce the strict `created_at` window on [`TIMED_KINDS`] — or stop.
    pub async fn set_window(&self, window: Option<Window>) {
        *self.state.window.lock().await = window;
    }

    /// Offset the relay's clock from the host's by `secs` (a skew a client
    /// must learn from the rejections).
    pub async fn set_skew(&self, secs: i64) {
        *self.state.skew.lock().await = secs;
    }

    /// Require the NIP-13 `nonce` tag on [`POW_KINDS`], mined to `min`
    /// leading zero bits; `0` requires nothing.
    pub async fn set_min_pow(&self, min: u8) {
        *self.state.min_pow.lock().await = min;
    }

    /// The NIP-11 document this relay would advertise for its current
    /// window and proof of work.
    pub async fn info(&self) -> RelayInfo {
        let window = *self.state.window.lock().await;
        let min_pow = *self.state.min_pow.lock().await;
        RelayInfo {
            name: Some("mini relay".to_owned()),
            software: None,
            limitation: Limitation {
                created_at_lower_limit: window.map(|w| w.past),
                created_at_upper_limit: window.map(|w| w.future),
                created_at_window_kinds: window
                    .map(|_| TIMED_KINDS.iter().map(|k| *k as u16).collect()),
                min_pow_difficulty: (min_pow > 0).then_some(min_pow),
            },
        }
    }

    /// The relay's clock now, in unix seconds (the host's, plus the skew).
    pub async fn now(&self) -> u64 {
        let skew = *self.state.skew.lock().await;
        let host = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        u64::try_from(i64::try_from(host).unwrap_or(i64::MAX).saturating_add(skew)).unwrap_or(0)
    }

    /// Inject an already-signed event as though a client had published it
    /// (stored + broadcast) — how the test acts as matchmaker and opponent.
    pub async fn inject(&self, event: Value) {
        store_and_broadcast(&self.state, event).await;
    }

    /// Snapshot of every EVENT frame received so far (swallowed included).
    pub async fn received(&self) -> Vec<ReceivedEvent> {
        self.state.received.lock().await.clone()
    }

    /// Snapshot of the stored events (what a fetching client can see).
    pub async fn stored(&self) -> Vec<Value> {
        self.state.stored.lock().await.clone()
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<RelayState>) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = ws.split();
    // One writer task per connection; REQ handlers and broadcasts feed it.
    let (outbound, mut outbox) = unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(text) = outbox.recv().await {
            if sink
                .send(tokio_tungstenite::tungstenite::Message::text(text))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(Ok(message)) = source.next().await {
        let Ok(text) = message.into_text() else {
            continue;
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(items) = frame.as_array() else {
            continue;
        };
        match items.first().and_then(Value::as_str) {
            Some("EVENT") => {
                if let Some(event) = items.get(1) {
                    handle_publish(&state, event.clone(), &outbound).await;
                }
            }
            Some("REQ") => {
                let Some(sub_id) = items.get(1).and_then(Value::as_str) else {
                    continue;
                };
                let filters: Vec<Value> = items.iter().skip(2).cloned().collect();
                // Replay the store, then EOSE, then keep the subscription live.
                for event in state.stored.lock().await.iter() {
                    if filters.iter().any(|filter| matches(event, filter)) {
                        let _ = outbound.send(json!(["EVENT", sub_id, event]).to_string());
                    }
                }
                let _ = outbound.send(json!(["EOSE", sub_id]).to_string());
                state.subscriptions.lock().await.push(Subscription {
                    sub_id: sub_id.to_owned(),
                    filters,
                    outbound: outbound.clone(),
                });
            }
            Some("CLOSE") => {
                if let Some(sub_id) = items.get(1).and_then(Value::as_str) {
                    state.subscriptions.lock().await.retain(|sub| {
                        !(sub.sub_id == sub_id && sub.outbound.same_channel(&outbound))
                    });
                }
            }
            _ => {}
        }
    }
}

async fn handle_publish(state: &Arc<RelayState>, event: Value, outbound: &UnboundedSender<String>) {
    let id = event
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let pubkey = event
        .get("pubkey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let kind = event.get("kind").and_then(Value::as_u64).unwrap_or(0);
    let content = event
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let step = event
        .get("tags")
        .and_then(Value::as_array)
        .and_then(|tags| {
            tags.iter().find_map(|tag| {
                let tag = tag.as_array()?;
                if tag.first()?.as_str()? == "step" {
                    Some(tag.get(1)?.as_str()?.to_owned())
                } else {
                    None
                }
            })
        });
    state.received.lock().await.push(ReceivedEvent {
        pubkey: pubkey.clone(),
        kind,
        content,
        step,
        at: Instant::now(),
    });

    // The policy: the strict window on the timed kinds, the proof of work
    // on the kinds that prescribe it — with the reference relay's wordings.
    if let Some(reason) = rejection(state, &event, kind).await {
        let _ = outbound.send(json!(["OK", id, false, reason]).to_string());
        return;
    }

    // The fault: acknowledge, deliver to no one. The publisher believes the
    // relay took it; every reader (the publisher's own fetches included)
    // never sees it.
    let swallowed = kind == PLY_KIND
        && state
            .swallow_plies_from
            .lock()
            .await
            .as_deref()
            .is_some_and(|target| target == pubkey);
    let _ = outbound.send(json!(["OK", id, true, ""]).to_string());
    if !swallowed {
        store_and_broadcast(state, event).await;
    }
}

/// The reference relay's decision on `event`, when it rejects it.
async fn rejection(state: &Arc<RelayState>, event: &Value, kind: u64) -> Option<String> {
    if let Some(window) = *state.window.lock().await {
        if TIMED_KINDS.contains(&kind) {
            let created_at = event.get("created_at").and_then(Value::as_i64).unwrap_or(0);
            let skew = *state.skew.lock().await;
            let host = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let now = i64::try_from(host).unwrap_or(i64::MAX).saturating_add(skew);
            if created_at < now.saturating_sub(i64::try_from(window.past).unwrap_or(i64::MAX)) {
                return Some(format!(
                    "invalid: created_at {created_at} is in the past (relay clock {now}, tolerance {}s)",
                    window.past
                ));
            }
            if created_at > now.saturating_add(i64::try_from(window.future).unwrap_or(i64::MAX)) {
                return Some(format!(
                    "invalid: timestamp {created_at} is too far in the future (relay clock {now}, tolerance {}s)",
                    window.future
                ));
            }
        }
    }
    let min = *state.min_pow.lock().await;
    if min > 0 && POW_KINDS.contains(&kind) {
        let nonces: Vec<&Vec<Value>> = event
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_array)
                    .filter(|tag| tag.first().and_then(Value::as_str) == Some("nonce"))
                    .collect()
            })
            .unwrap_or_default();
        let [nonce] = nonces.as_slice() else {
            return Some(if nonces.is_empty() {
                format!("pow: kind {kind} requires a NIP-13 nonce tag (minimum difficulty {min})")
            } else {
                format!("pow: kind {kind} requires exactly one NIP-13 nonce tag")
            });
        };
        let Some(committed) = nonce
            .get(2)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u8>().ok())
        else {
            return Some(format!(
                "pow: the nonce tag must commit to a target difficulty (minimum {min})"
            ));
        };
        if committed < min {
            return Some(format!(
                "pow: committed target {committed} is below the required minimum {min}"
            ));
        }
        let achieved = leading_zero_bits(event.get("id").and_then(Value::as_str).unwrap_or(""));
        if achieved < u32::from(min) {
            return Some(format!(
                "pow: difficulty {achieved} is below the required minimum {min}"
            ));
        }
    }
    None
}

/// The NIP-13 difficulty of an id: its leading zero bits.
fn leading_zero_bits(id_hex: &str) -> u32 {
    let mut bits = 0;
    for c in id_hex.chars() {
        let Some(nibble) = c.to_digit(16) else {
            break;
        };
        if nibble == 0 {
            bits += 4;
        } else {
            bits += nibble.leading_zeros() - 28;
            break;
        }
    }
    bits
}

async fn store_and_broadcast(state: &Arc<RelayState>, event: Value) {
    state.stored.lock().await.push(event.clone());
    let subscriptions = state.subscriptions.lock().await;
    for sub in subscriptions.iter() {
        if sub.filters.iter().any(|filter| matches(&event, filter)) {
            let _ = sub
                .outbound
                .send(json!(["EVENT", sub.sub_id, event]).to_string());
        }
    }
}

/// NIP-01 filter matching — the subset a client's REQs use.
fn matches(event: &Value, filter: &Value) -> bool {
    let Some(fields) = filter.as_object() else {
        return false;
    };
    for (key, value) in fields {
        let hit = match key.as_str() {
            "ids" => prefix_match(event.get("id"), value),
            "authors" => prefix_match(event.get("pubkey"), value),
            "kinds" => value.as_array().is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|k| k == event.get("kind").unwrap_or(&Value::Null))
            }),
            "since" => {
                event.get("created_at").and_then(Value::as_u64).unwrap_or(0)
                    >= value.as_u64().unwrap_or(0)
            }
            "until" => {
                event
                    .get("created_at")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX)
                    <= value.as_u64().unwrap_or(u64::MAX)
            }
            "limit" => true, // bounded store; a test never needs windowing
            tag_key if tag_key.starts_with('#') && tag_key.len() == 2 => {
                let name = &tag_key[1..];
                let wanted = value.as_array().cloned().unwrap_or_default();
                event
                    .get("tags")
                    .and_then(Value::as_array)
                    .is_some_and(|tags| {
                        tags.iter().any(|tag| {
                            tag.as_array().is_some_and(|tag| {
                                tag.first().and_then(Value::as_str) == Some(name)
                                    && tag.get(1).is_some_and(|v| wanted.contains(v))
                            })
                        })
                    })
            }
            _ => true, // unknown key: permissive (a test favours delivery)
        };
        if !hit {
            return false;
        }
    }
    true
}

fn prefix_match(actual: Option<&Value>, wanted: &Value) -> bool {
    let Some(actual) = actual.and_then(Value::as_str) else {
        return false;
    };
    wanted.as_array().is_some_and(|list| {
        list.iter()
            .filter_map(Value::as_str)
            .any(|prefix| actual.starts_with(prefix))
    })
}
