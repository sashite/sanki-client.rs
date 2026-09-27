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
