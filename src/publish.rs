// SPDX-License-Identifier: Apache-2.0
//! Self-timed publishing discipline (Nostr Integration §Client obligations
//! in self-timed mode) + NIP-13 mining.
//!
//! Every event a client publishes to the strict timing relay is stamped with
//! its best estimate of the **relay's** clock plus a minimal forward buffer —
//! never blindly the local clock — and mined to the configured NIP-13
//! difficulty. A rejection whose reason carries the `created_at` token means
//! the stamp was stale: the event was never stored, carries no penalty, and
//! is simply re-signed with a corrected `created_at` — the relay's clock
//! learnt exactly when the reason states it (the reference relay's `(relay
//! clock M, tolerance Ts)`), else the estimate stepped its way. Any other
//! rejection is surfaced, never blind-retried.

use std::num::NonZeroU8;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{anyhow, Result};
use nostr_sdk::prelude::*;

/// Forward buffer over the estimated relay clock (seconds). Kept minimal:
/// any surplus is charged to the mover as elapsed time.
const FORWARD_BUFFER_SECS: u64 = 1;

/// Per-retry step on a timing rejection whose reason does not state the
/// relay's clock (seconds).
const RETRY_BUMP_SECS: i64 = 2;

/// Cap on the maintained skew estimate (seconds, either direction).
const MAX_SKEW_SECS: i64 = 60;

/// Timing attempts per publish before giving up.
const MAX_TIMING_ATTEMPTS: u32 = 6;

/// The per-connection relay-clock skew estimate (relay − local), signed.
/// Shared by every publish of one client; starts at zero (local UTC).
#[derive(Debug, Default)]
pub struct RelayClock {
    skew_secs: AtomicI64,
}

impl RelayClock {
    /// A fresh estimate (no observed skew).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            skew_secs: AtomicI64::new(0),
        }
    }

    /// The relay's clock as estimated now, in unix seconds — the instant
    /// every timing comparison with the relay's events is made at (a cutoff,
    /// a deadline), so that a host clock behind the relay never makes the
    /// client believe it has more time than it has.
    #[must_use]
    pub fn now_secs(&self) -> u64 {
        let now = Timestamp::now().as_secs();
        let skew = self.skew_secs.load(Ordering::Relaxed);
        let estimated = i64::try_from(now).unwrap_or(i64::MAX).saturating_add(skew);
        u64::try_from(estimated.max(0)).unwrap_or(0)
    }

    /// The `created_at` to stamp right now: the relay's clock plus the
    /// forward buffer.
    #[must_use]
    pub fn stamp(&self) -> Timestamp {
        Timestamp::from_secs(self.now_secs().saturating_add(FORWARD_BUFFER_SECS))
    }

    /// Raise the estimate after a stale rejection.
    pub(crate) fn bump(&self) {
        let _ = self
            .skew_secs
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |skew| {
                Some(skew.saturating_add(RETRY_BUMP_SECS).min(MAX_SKEW_SECS))
            });
    }

    /// Lower the estimate after a too-far-future rejection.
    pub(crate) fn lower(&self) {
        let _ = self
            .skew_secs
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |skew| {
                Some(skew.saturating_sub(RETRY_BUMP_SECS).max(-MAX_SKEW_SECS))
            });
    }

    /// Learn the relay's clock from a rejection that states it (the
    /// reference relay's `(relay clock M, tolerance Ts)`): the skew is
    /// `M − host`, exactly, within the cap — `host` being the host's clock
    /// as read when the event was sent, so that the round trip does not
    /// bias the estimate low.
    pub(crate) fn learn(&self, relay_now: u64, host_at_send: u64) {
        let host = i64::try_from(host_at_send).unwrap_or(i64::MAX);
        let relay = i64::try_from(relay_now).unwrap_or(i64::MAX);
        let skew = relay
            .saturating_sub(host)
            .clamp(-MAX_SKEW_SECS, MAX_SKEW_SECS);
        self.skew_secs.store(skew, Ordering::Relaxed);
    }

    /// The learnt skew, in seconds (relay − host).
    #[must_use]
    pub fn skew_secs(&self) -> i64 {
        self.skew_secs.load(Ordering::Relaxed)
    }
}

/// The relay's clock, when a timing rejection states it: the reference
/// relay writes `(relay clock M, tolerance Ts)`.
#[must_use]
pub fn relay_clock_in(reason: &str) -> Option<u64> {
    let (_, rest) = reason.split_once("relay clock ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether a relay rejection reason denotes a STALE `created_at` (the strict
/// relay's contract: the token `created_at` appears in the reason). A
/// future-dated rejection deliberately does NOT carry the token; recognize
/// its usual wordings separately so the skew moves the right way.
#[must_use]
pub fn is_stale_reason(reason: &str) -> bool {
    reason.contains("created_at") && !is_future_reason(reason)
}

/// Whether a rejection reason denotes a too-far-future `created_at`.
#[must_use]
pub fn is_future_reason(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    lower.contains("future") || lower.contains("ahead") || lower.contains("too far forward")
}

/// Whether, and how hard, an event is mined (NIP-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pow {
    /// The kind prescribes no `nonce` tag (a profile, a Game Session, a
    /// reaction): none is added.
    None,
    /// The kind prescribes a `nonce` tag (every player-published founding,
    /// Ply and Conclusion): mined to this difficulty — `0` adds the
    /// trivially-satisfied `["nonce", "0", "0"]`.
    Mined(u8),
}

/// Build, mine, sign and publish an event whose tags may depend on the
/// stamped `created_at` (e.g. an `accept_until` window). Retries stale and
/// future rejections with a corrected stamp; returns the accepted event.
///
/// # Errors
///
/// A send that could not start, a rejection that is not a timing one
/// (`pow:`, `rate-limited:`, `blocked:`, another `invalid:`), or a timing
/// the relay kept rejecting.
pub async fn publish_self_timed<F>(
    client: &Client,
    keys: &Keys,
    relay_clock: &RelayClock,
    kind: Kind,
    pow: Pow,
    build: F,
) -> Result<Event>
where
    F: Fn(Timestamp) -> (Vec<Tag>, String),
{
    for _attempt in 0..MAX_TIMING_ATTEMPTS {
        let created_at = relay_clock.stamp();
        let (mut tags, content) = build(created_at);
        let mut builder = EventBuilder::new(kind, content).custom_created_at(created_at);
        let mut difficulty: Option<NonZeroU8> = None;
        match pow {
            Pow::Mined(target) if target > 0 => {
                // Mining adds the NIP-13 `nonce` tag as a side effect. Since
                // nostr 0.45 it happens on the UNSIGNED event rather than on
                // the builder (`EventBuilder::pow` is gone), so the target is
                // carried down to the signing step below.
                difficulty = NonZeroU8::new(target);
                builder = builder.tags(tags);
            }
            Pow::Mined(_) => {
                // The `nonce` tag is STRUCTURALLY required on a player-published
                // suite event — a Ply (kind 3423, constraint 6), a Conclusion, a
                // founding — independently of the relay's difficulty policy: a
                // conforming consumer rejects one that carries none, so a Ply
                // without it never joins the canonical chain and the board
                // wedges. At difficulty 0 (a dev relay enforcing no PoW) mining
                // is skipped, so the tag would be absent; add the trivially-
                // satisfied 0-target nonce explicitly, mirroring the app's own
                // miner, which emits `["nonce", "0", "0"]` at difficulty 0.
                tags.push(Tag::custom("nonce", ["0", "0"]));
                builder = builder.tags(tags);
            }
            Pow::None => builder = builder.tags(tags),
        }
        let unsigned = builder.finalize_unsigned(keys.public_key());
        let unsigned = match difficulty {
            Some(target) => unsigned
                .mine(&SingleThreadPow, target)
                .map_err(|e| anyhow!("mining failed: {e}"))?,
            None => unsigned,
        };
        let event = unsigned
            .finalize(keys)
            .map_err(|e| anyhow!("signing failed: {e}"))?;

        // Since nostr-sdk 0.41 `send_event` answers `Ok` whether or not any
        // relay accepted the event: a relay's `OK false` is in `failed`, keyed
        // by relay, with the relay's message verbatim. An `Err` is a send that
        // could not start (no relay, a shut-down client).
        let sent_at = Timestamp::now().as_secs();
        let output = client
            .send_event(&event)
            .await
            .map_err(|e| anyhow!("sending failed: {e}"))?;
        if !output.success.is_empty() {
            return Ok(event);
        }
        let reason = output
            .failed
            .values()
            .next()
            .cloned()
            .unwrap_or_else(|| "no relay answered".to_owned());
        if is_stale_reason(&reason) {
            match relay_clock_in(&reason) {
                Some(relay_now) => relay_clock.learn(relay_now, sent_at),
                None => relay_clock.bump(),
            }
            tracing::debug!(%reason, "stale created_at; re-signing with a bumped stamp");
            continue;
        }
        if is_future_reason(&reason) {
            match relay_clock_in(&reason) {
                Some(relay_now) => relay_clock.learn(relay_now, sent_at),
                None => relay_clock.lower(),
            }
            tracing::debug!(%reason, "future created_at; re-signing with a lowered stamp");
            continue;
        }
        return Err(anyhow!("relay rejected the event: {reason}"));
    }
    Err(anyhow!("relay kept rejecting the event's timing"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn classifies_rejection_reasons() {
        assert!(is_stale_reason("invalid: created_at is in the past"));
        assert!(!is_stale_reason("blocked: too many events"));
        // The future wording moves the skew the other way, even when a relay
        // (non-conformingly) includes the token.
        assert!(is_future_reason("invalid: timestamp too far in the future"));
        assert!(!is_stale_reason("invalid: created_at too far ahead"));
    }

    #[test]
    fn the_relay_clock_is_read_from_the_reference_wording() {
        assert_eq!(
            relay_clock_in(
                "invalid: created_at 100 is in the past (relay clock 105, tolerance 1s)"
            ),
            Some(105)
        );
        assert_eq!(
            relay_clock_in(
                "invalid: timestamp 120 is too far in the future (relay clock 105, tolerance 5s)"
            ),
            Some(105)
        );
        assert_eq!(relay_clock_in("invalid: created_at is stale"), None);
        let clock = RelayClock::new();
        clock.learn(1_000, 1_003);
        assert_eq!(clock.skew_secs(), -3);
        clock.learn(1_000, 900);
        assert_eq!(clock.skew_secs(), 60, "capped");
    }

    #[test]
    fn skew_is_bounded_both_ways() {
        let clock = RelayClock::new();
        for _ in 0..100 {
            clock.bump();
        }
        assert_eq!(clock.skew_secs.load(Ordering::Relaxed), MAX_SKEW_SECS);
        for _ in 0..200 {
            clock.lower();
        }
        assert_eq!(clock.skew_secs.load(Ordering::Relaxed), -MAX_SKEW_SECS);
    }

    #[test]
    fn stamp_carries_the_forward_buffer() {
        let clock = RelayClock::new();
        let now = Timestamp::now().as_secs();
        let stamped = clock.stamp().as_secs();
        assert!(stamped >= now + FORWARD_BUFFER_SECS);
        assert!(stamped <= now + FORWARD_BUFFER_SECS + 2);
    }
}
