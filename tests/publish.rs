// SPDX-License-Identifier: Apache-2.0
//! The self-timed publishing discipline against the in-process relay: the
//! strict window with a skewed relay clock, and the proof of work.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use nostr_sdk::prelude::*;
use sashite_sanki_client::publish::{publish_self_timed, Pow, RelayClock};
use sashite_sanki_client::relay::RelayInfo;
use sashite_sanki_client::testing::{MiniRelay, Window};
use std::time::Duration;

async fn connected(relay: &MiniRelay) -> Client {
    let client = Client::builder().build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().and_wait(Duration::from_secs(5)).await;
    client
}

fn ply_tags(_stamp: Timestamp) -> (Vec<Tag>, String) {
    (
        vec![Tag::parse(["step", "1"]).unwrap()],
        r#"["e2","e4",null]"#.to_owned(),
    )
}

#[tokio::test]
async fn a_stale_stamp_is_learnt_from_the_relays_rejection() {
    let relay = MiniRelay::start().await;
    relay.set_window(Some(Window::REFERENCE)).await;
    // The relay's clock runs 4 s ahead of the host's: a stamp of `now + 1`
    // is 3 s in the relay's past, beyond its 1 s tolerance.
    relay.set_skew(4).await;
    let client = connected(&relay).await;
    let keys = Keys::generate();
    let clock = RelayClock::new();

    let event = publish_self_timed(
        &client,
        &keys,
        &clock,
        Kind::Custom(3423),
        Pow::Mined(0),
        ply_tags,
    )
    .await
    .expect("the stamp is bumped until the relay accepts it");

    // Accepted, stored, and stamped within the relay's window.
    let stored = relay.stored().await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["id"].as_str().unwrap(), event.id.to_hex());
    let relay_now = relay.now().await;
    assert!(event.created_at.as_secs() + 1 >= relay_now);
    assert!(event.created_at.as_secs() <= relay_now + 5);
    // The relay saw the rejected attempts too: at least one before the
    // accepted one.
    assert!(relay.received().await.len() >= 2);
    // The skew is learnt, as far as acceptance needed: the estimate moved
    // toward the relay's clock, and the next stamp lands in the window.
    let host_now = Timestamp::now().as_secs();
    assert!(clock.now_secs() > host_now);
    assert!(clock.stamp().as_secs() + 1 >= relay_now);
    client.disconnect().await;
}

#[tokio::test]
async fn a_future_stamp_is_lowered() {
    let relay = MiniRelay::start().await;
    relay.set_window(Some(Window::REFERENCE)).await;
    // The relay's clock runs 8 s behind: `now + 1` is 9 s in its future.
    relay.set_skew(-8).await;
    let client = connected(&relay).await;
    let keys = Keys::generate();
    let clock = RelayClock::new();

    let event = publish_self_timed(
        &client,
        &keys,
        &clock,
        Kind::Custom(3423),
        Pow::Mined(0),
        ply_tags,
    )
    .await
    .expect("the stamp is lowered until the relay accepts it");
    let relay_now = relay.now().await;
    assert!(event.created_at.as_secs() <= relay_now + 5);
    assert!(relay.received().await.len() >= 2);
    client.disconnect().await;
}

#[tokio::test]
async fn the_nonce_is_structural_and_mined_to_the_minimum() {
    let relay = MiniRelay::start().await;
    relay.set_min_pow(8).await;
    let client = connected(&relay).await;
    let keys = Keys::generate();
    let clock = RelayClock::new();

    // Mined to the advertised minimum: accepted.
    let min = relay.info().await.min_pow_difficulty();
    assert_eq!(min, 8);
    let event = publish_self_timed(
        &client,
        &keys,
        &clock,
        Kind::Custom(3423),
        Pow::Mined(min),
        ply_tags,
    )
    .await
    .unwrap();
    assert!(event
        .tags
        .iter()
        .any(|tag| tag.as_slice().first().map(String::as_str) == Some("nonce")));
    assert!(sashite_sanki_client::session::pow_ok(&event));

    // The trivially-satisfied nonce at difficulty 0: refused by this relay
    // (below the minimum), and the refusal is not a timing one, so it is
    // surfaced rather than retried.
    let err = publish_self_timed(
        &client,
        &keys,
        &clock,
        Kind::Custom(3423),
        Pow::Mined(0),
        ply_tags,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("pow:"), "{err}");

    // A kind that prescribes no nonce is not asked for one.
    publish_self_timed(
        &client,
        &keys,
        &clock,
        Kind::Custom(3422),
        Pow::None,
        |_| (Vec::new(), "session".to_owned()),
    )
    .await
    .unwrap();
    assert_eq!(relay.stored().await.len(), 2);
    client.disconnect().await;
}

#[tokio::test]
async fn the_relay_advertises_what_it_enforces() {
    let relay = MiniRelay::start().await;
    let open: RelayInfo = relay.info().await;
    assert!(open.check_self_timed().is_err());
    relay.set_window(Some(Window::REFERENCE)).await;
    let strict = relay.info().await;
    assert_eq!(strict.check_self_timed(), Ok(()));
    assert_eq!(strict.past_tolerance(), Some(1));
    assert!(strict.covers(3420) && !strict.covers(0));
}
