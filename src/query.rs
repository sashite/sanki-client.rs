// SPDX-License-Identifier: Apache-2.0
//! A query that proves something (ADR-0045 §8 *Nothing is written on an
//! unknown state*): a `REQ` sent to the relay itself, answered by the
//! events **and the `EOSE`** for that subscription, under a bound.
//!
//! The pool's `fetch_events` is not that proof: its stream ends silently
//! on a timeout, on a disconnection mid-query (`nostr-sdk` 0.45 returns
//! the events received so far as if the relay had said `EOSE`), and on a
//! lagged notification channel. Here the notifications are subscribed
//! **before** the `REQ` is sent, the `EOSE` (or a `CLOSED` without a
//! machine-readable prefix, the relay's "nothing more will come") is
//! required, and a shutdown, an error `CLOSED` or the bound answer
//! `None`: nothing is proven. (A lag of the notification channel cannot
//! be seen through the pool's stream, which skips it; the loop below does
//! nothing but collect, and the channel holds thousands of messages, so a
//! lag takes a relay flooding the client during the bound.)

use std::collections::HashSet;
use std::time::Duration;

use futures_util::StreamExt;
use nostr_sdk::prelude::*;

/// The bound on one query.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// The events `filters` match on `relay`, once the relay said so — or
/// `None` when it did not within `timeout`. Every event's signature is
/// verified; duplicates are dropped.
pub async fn query(
    client: &Client,
    relay: &RelayUrl,
    filters: Vec<Filter>,
    timeout: Duration,
) -> Option<Vec<Event>> {
    let handle = client.relay(relay).await.ok()??;
    if !handle.status().is_connected() {
        return None;
    }
    // The channel first, the REQ after: nothing the relay answers is missed.
    let mut notifications = client.notifications();
    let id = SubscriptionId::generate();
    let close_on = SubscribeAutoCloseOptions::default()
        .exit_policy(ReqExitPolicy::ExitOnEOSE)
        .timeout(Some(timeout));
    handle
        .subscribe(filters)
        .with_id(id.clone())
        .close_on(close_on)
        .await
        .ok()?;
    let answer = tokio::time::timeout(timeout, async {
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        loop {
            match notifications.next().await {
                Some(ClientNotification::Message { relay_url, message }) if relay_url == *relay => {
                    match *message {
                        RelayMessage::Event {
                            subscription_id,
                            event,
                        } if *subscription_id == id => {
                            let event = event.into_owned();
                            if event.verify().is_ok() && seen.insert(event.id) {
                                events.push(event);
                            }
                        }
                        RelayMessage::EndOfStoredEvents(subscription_id)
                            if *subscription_id == id =>
                        {
                            return Some(events);
                        }
                        RelayMessage::Closed {
                            subscription_id,
                            message,
                        } if *subscription_id == id => {
                            // "Nothing more will come" proves as much as
                            // EOSE; an error proves nothing.
                            return MachineReadablePrefix::parse(&message)
                                .is_none()
                                .then_some(events);
                        }
                        _ => {}
                    }
                }
                Some(ClientNotification::Shutdown) | None => return None,
                Some(_) => {}
            }
        }
    })
    .await
    .ok()
    .flatten();
    let _ = handle.unsubscribe(&id).await;
    answer
}
