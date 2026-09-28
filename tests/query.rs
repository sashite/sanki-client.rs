// SPDX-License-Identifier: Apache-2.0
//! A proven query against the in-process relay: the events with their
//! EOSE, an empty answer with its EOSE, and silence.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::time::Duration;

use nostr_sdk::prelude::*;
use sashite_sanki_client::query::query;
use sashite_sanki_client::testing::MiniRelay;

async fn connected(relay: &MiniRelay) -> Client {
    let client = Client::builder().build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().and_wait(Duration::from_secs(5)).await;
    client
}

#[tokio::test]
async fn the_eose_proves_the_answer_and_silence_proves_nothing() {
    let relay = MiniRelay::start().await;
    let client = connected(&relay).await;
    let url = RelayUrl::parse(&relay.url).unwrap();
    let keys = Keys::generate();
    let bound = Duration::from_secs(2);

    // Nothing stored: an empty, proven answer.
    let filter = Filter::new().author(keys.public_key()).kind(Kind::TextNote);
    assert_eq!(
        query(&client, &url, vec![filter.clone()], bound).await,
        Some(Vec::new())
    );

    // Two notes, one of them twice on the wire: two events, verified.
    for i in 0..2u64 {
        let note = EventBuilder::new(Kind::TextNote, format!("note {i}"))
            .custom_created_at(Timestamp::from(1_700_000_000 + i))
            .finalize(&keys)
            .unwrap();
        relay
            .inject(serde_json::from_str(&note.as_json()).unwrap())
            .await;
        if i == 0 {
            relay
                .inject(serde_json::from_str(&note.as_json()).unwrap())
                .await;
        }
    }
    let answer = query(&client, &url, vec![filter.clone()], bound)
        .await
        .unwrap();
    assert_eq!(answer.len(), 2);

    // A forged event is dropped.
    let mut forged: serde_json::Value = serde_json::from_str(
        &EventBuilder::new(Kind::TextNote, "forged")
            .finalize(&keys)
            .unwrap()
            .as_json(),
    )
    .unwrap();
    forged["content"] = serde_json::Value::String("edited".to_owned());
    relay.inject(forged).await;
    let answer = query(&client, &url, vec![filter.clone()], bound)
        .await
        .unwrap();
    assert_eq!(answer.len(), 2);

    // A silent relay: nothing proven.
    relay.set_silent(true).await;
    assert_eq!(query(&client, &url, vec![filter], bound).await, None);

    // A relay the client does not know: nothing proven.
    let unknown = RelayUrl::parse("wss://nowhere.example.com").unwrap();
    assert_eq!(query(&client, &unknown, vec![], bound).await, None);
}

#[tokio::test]
async fn a_cut_proves_nothing_and_the_restore_answers_again() {
    let relay = MiniRelay::start().await;
    let client = Client::builder().build();
    // A short retry, so that the test does not wait for the default.
    client
        .add_relay(&relay.url)
        .opts(RelayOptions::new().retry_interval(Duration::from_secs(1)))
        .await
        .unwrap();
    client.connect().and_wait(Duration::from_secs(5)).await;
    let url = RelayUrl::parse(&relay.url).unwrap();
    let keys = Keys::generate();
    let bound = Duration::from_secs(2);
    let filter = Filter::new().author(keys.public_key()).kind(Kind::TextNote);
    assert_eq!(
        query(&client, &url, vec![filter.clone()], bound).await,
        Some(Vec::new())
    );

    // The cut: the connection drops, and nothing is proven while it lasts.
    relay.cut().await;
    let note = EventBuilder::new(Kind::TextNote, "during the cut")
        .finalize(&keys)
        .unwrap();
    relay
        .inject(serde_json::from_str(&note.as_json()).unwrap())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let relay_handle = client.relay(&url).await.unwrap().unwrap();
    assert!(
        !relay_handle.status().is_connected(),
        "the cut closed the socket"
    );
    assert_eq!(
        query(&client, &url, vec![filter.clone()], bound).await,
        None
    );

    // The restore: the client reconnects on its own, and the query answers
    // what the relay stored during the cut.
    relay.restore().await;
    let mut answer = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !relay_handle.status().is_connected() {
            continue;
        }
        answer = query(&client, &url, vec![filter.clone()], bound).await;
        if answer.is_some() {
            break;
        }
    }
    assert_eq!(answer.map(|events| events.len()), Some(1));
}
