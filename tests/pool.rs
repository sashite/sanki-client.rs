// SPDX-License-Identifier: Apache-2.0
//! The pool's readers and drafts, end to end through the wire's shapes: an
//! entry drafted by this crate reads back as a conforming Open Challenge,
//! and the scripted matchmaker of `testing` pairs two of them into a
//! Pairing the reader accepts — or refuses, when told to misbehave.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use nostr_sdk::prelude::*;
use sashite_sanki_client::drafts::{self, Publishable};
use sashite_sanki_client::readers::{self, Filter};
use sashite_sanki_client::testing;

const RELAY: &str = "wss://relay.sanki.app";

fn entry_of(keys: &Keys, matchmaker: &PublicKey, variant: &str, stamp: u64) -> Event {
    let draft = drafts::OpenChallenge {
        matchmaker: *matchmaker,
        timing_relay: RELAY.to_owned(),
        game: "sanki".to_owned(),
        rules: EventId::from_slice(&[7; 32]).unwrap(),
        variant: variant.to_owned(),
        time_control: vec![[Some(180), Some(2), None]],
        filter: Filter::Everyone,
        accept_until: stamp + 60,
        not_after: stamp + 50,
    };
    let parts = draft.at(stamp, RELAY).unwrap();
    let mut tags = parts.tags;
    tags.push(Tag::custom("nonce", ["0", "0"]));
    tags.push(Tag::custom("client", ["sanki-bot"]));
    EventBuilder::new(draft.kind(), parts.content)
        .tags(tags)
        .custom_created_at(Timestamp::from(stamp))
        .finalize(keys)
        .unwrap()
}

#[test]
fn a_drafted_entry_reads_back_and_pairs() {
    let matchmaker = Keys::generate();
    let alice = Keys::generate();
    let bot = Keys::generate();
    let a =
        readers::open_challenge(&entry_of(&alice, &matchmaker.public_key(), "ogi", 1_000)).unwrap();
    let b =
        readers::open_challenge(&entry_of(&bot, &matchmaker.public_key(), "ogi", 1_010)).unwrap();
    assert_eq!(a.filter, Filter::Everyone);
    assert_eq!(a.self_variant.as_deref(), Some("ogi"));
    assert!(readers::has_client_tag(
        &entry_of(&bot, &matchmaker.public_key(), "ogi", 1_010),
        "sanki-bot"
    ));
    let pairing = testing::pairing_of(
        &matchmaker,
        &a,
        &b,
        &bot.public_key(),
        ("ogi", "ogi"),
        1_020,
        1_140,
    );
    let p = readers::pairing(&pairing, [&a, &b]).unwrap();
    assert_eq!((p.first, p.second), (bot.public_key(), alice.public_key()));
    assert_eq!(p.found_until, 1_140);
    // The matchmaker misbehaving: a variant against the bot's mirror term,
    // a Pairing after the person's accept_until, another matchmaker.
    let wrong = testing::pairing_of(
        &matchmaker,
        &a,
        &b,
        &bot.public_key(),
        ("ogi", "chess"),
        1_020,
        1_140,
    );
    assert_eq!(
        readers::pairing(&wrong, [&a, &b]).unwrap_err().reason,
        "a variant against the player's own term"
    );
    let late = testing::pairing_of(
        &matchmaker,
        &a,
        &b,
        &bot.public_key(),
        ("ogi", "ogi"),
        1_061,
        1_180,
    );
    assert_eq!(
        readers::pairing(&late, [&a, &b]).unwrap_err().reason,
        "after an entry's accept_until"
    );
    let impostor = testing::pairing_of(
        &Keys::generate(),
        &a,
        &b,
        &bot.public_key(),
        ("ogi", "ogi"),
        1_020,
        1_140,
    );
    assert_eq!(
        readers::pairing(&impostor, [&a, &b]).unwrap_err().reason,
        "not signed by the entries' matchmaker"
    );
}
