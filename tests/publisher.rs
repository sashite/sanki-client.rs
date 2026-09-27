// SPDX-License-Identifier: Apache-2.0
//! The Publisher against the in-process relay: stamping, the window and a
//! skew, the proof of work, the reserve, the outcomes and what each
//! convergence does with a missing acknowledgment, the lease.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;
use std::time::Duration;

use nostr_sdk::prelude::*;
use sashite_sanki_client::drafts::{
    ChallengePolicy, Conclusion, DirectChallenge, GameSession, Ply, Policy, Profile, SessionPlan,
    Window, Withheld,
};
use sashite_sanki_client::module::{SeatResult, Verdict};
use sashite_sanki_client::publisher::{
    OpenError, Outcome, Publisher, Rejection, Resolution, Settings, CLIENT_TAG,
};
use sashite_sanki_client::readers::{self, Founding, PolicyMode};
use sashite_sanki_client::session;
use sashite_sanki_client::testing::{MiniRelay, Window as RelayWindow};

async fn connected(relay: &MiniRelay) -> Client {
    let client = Client::builder().build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().and_wait(Duration::from_secs(5)).await;
    client
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sanki-client-publisher-{}-{}",
        std::process::id(),
        Timestamp::now().as_secs()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn settings(relay: &MiniRelay, rate: u32, pow: u8) -> Settings {
    Settings {
        relay: RelayUrl::parse(&relay.url).unwrap(),
        rate_per_minute: rate,
        data_dir: temp_dir(),
        pow,
        past_tolerance: 1,
        future_tolerance: 5,
    }
}

async fn open(relay: &MiniRelay, keys: Keys, rate: u32, pow: u8) -> Publisher<Keys> {
    let client = connected(relay).await;
    Publisher::open(client, keys, settings(relay, rate, pow))
        .await
        .unwrap()
}

fn id(byte: u8) -> EventId {
    EventId::from_slice(&[byte; 32]).unwrap()
}

fn ply(step: u32, now: u64) -> Ply {
    Ply {
        session: id(1),
        opponent: Keys::generate().public_key(),
        step,
        content: r#"["e2","e4",null]"#.to_owned(),
        draw: false,
        not_before: 0,
        not_after: now + 60,
    }
}

fn game_session(me: PublicKey, now: u64) -> GameSession {
    GameSession {
        plan: SessionPlan {
            founding: Founding::DirectChallenge(id(2)),
            game: "sanki".to_owned(),
            rules: id(7),
            timing_relay: "wss://relay.sanki.app".to_owned(),
            first: me,
            second: Keys::generate().public_key(),
            first_variant: "chess".to_owned(),
            second_variant: "chess".to_owned(),
            position: "x".to_owned(),
        },
        not_before: now - 10,
        not_after: now + 60,
    }
}

#[tokio::test]
async fn a_ply_is_stamped_mined_tagged_and_accepted() {
    let relay = MiniRelay::start().await;
    relay.set_window(Some(RelayWindow::REFERENCE)).await;
    relay.set_min_pow(8).await;
    let publisher = open(&relay, Keys::generate(), 30, 8).await;
    let now = publisher.now();
    let before = publisher.stamp();
    let outcome = publisher.publish(ply(1, now)).await;
    let Outcome::Accepted(event) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(event.created_at.as_secs() >= before);
    assert!(readers::has_client_tag(&event, CLIENT_TAG));
    assert!(session::pow_ok(&event));
    assert_eq!(relay.stored().await.len(), 1);
}

#[tokio::test]
async fn a_skewed_relay_is_learnt_and_an_expired_window_is_withheld() {
    let relay = MiniRelay::start().await;
    relay.set_window(Some(RelayWindow::REFERENCE)).await;
    relay.set_skew(4).await;
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    // Within a wide window: re-stamped until accepted.
    let outcome = publisher.publish(ply(1, now)).await;
    assert!(matches!(outcome, Outcome::Accepted(_)), "{outcome:?}");
    assert!(relay.received().await.len() >= 2);
    // Learnt: the next one goes through at once.
    let received = relay.received().await.len();
    assert!(matches!(
        publisher.publish(ply(2, now)).await,
        Outcome::Accepted(_)
    ));
    assert_eq!(relay.received().await.len(), received + 1);
}

#[tokio::test]
async fn a_stamp_past_not_after_is_expired() {
    let relay = MiniRelay::start().await;
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    let mut late = ply(1, now);
    late.not_after = now.saturating_sub(5);
    assert_eq!(
        publisher.publish(late).await,
        Outcome::Withheld(Withheld::Expired)
    );
    assert!(relay.received().await.is_empty());
}

#[tokio::test]
async fn not_before_is_waited_for() {
    let relay = MiniRelay::start().await;
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    let mut paced = ply(1, now);
    paced.not_before = now + 3;
    let started = std::time::Instant::now();
    let Outcome::Accepted(event) = publisher.publish(paced).await else {
        panic!()
    };
    // Stamped exactly at `not_before`, after a wait of at least a second
    // (two, unless the wall second ticked between `now()` and the choice).
    assert_eq!(event.created_at.as_secs(), now + 3);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn the_proof_of_work_below_the_minimum_is_rejected_finally() {
    let relay = MiniRelay::start().await;
    relay.set_min_pow(8).await;
    // The publisher believes the minimum is 0; NIP-11 is not served by
    // the mini relay, so the re-read learns nothing.
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    assert_eq!(
        publisher.publish(ply(1, now)).await,
        Outcome::Rejected(Rejection::Pow)
    );
    assert!(relay.received().await.len() >= 2, "re-mined at least once");
}

#[tokio::test]
async fn the_reserve_is_kept_from_standing_events() {
    let relay = MiniRelay::start().await;
    // Rate 3, reserve 1: standing events may spend two tokens.
    let publisher = open(&relay, Keys::generate(), 3, 0).await;
    let profile = || Profile {
        metadata: serde_json::Map::new(),
    };
    assert!(matches!(
        publisher.publish(profile()).await,
        Outcome::Accepted(_)
    ));
    assert!(matches!(
        publisher.publish(profile()).await,
        Outcome::Accepted(_)
    ));
    let third = tokio::time::timeout(Duration::from_secs(1), publisher.publish(profile())).await;
    assert!(
        third.is_err(),
        "the third standing event waits for the window"
    );
    // A Ply takes the last token.
    let now = publisher.now();
    let outcome = tokio::time::timeout(Duration::from_secs(2), publisher.publish(ply(1, now)))
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Accepted(_)), "{outcome:?}");
}

#[tokio::test]
async fn a_rate_limited_event_is_retried_then_final() {
    let relay = MiniRelay::start().await;
    relay.set_rate_limit(Some((1, 60))).await;
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    assert!(matches!(
        publisher.publish(ply(1, now)).await,
        Outcome::Accepted(_)
    ));
    let outcome = publisher.publish(ply(2, now)).await;
    assert_eq!(outcome, Outcome::Rejected(Rejection::RateLimited));
}

#[tokio::test]
async fn a_ply_without_acknowledgment_is_sent_again_unchanged() {
    let relay = MiniRelay::start().await;
    relay.set_drop_acks(1).await;
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    let Outcome::Accepted(event) = publisher.publish(ply(1, now)).await else {
        panic!()
    };
    // Two frames, the same event: the second was a duplicate the relay
    // acknowledged.
    let received = relay.received().await;
    assert_eq!(received.len(), 2);
    assert_eq!(relay.stored().await.len(), 1);
    assert_eq!(
        relay.stored().await[0]["id"].as_str().unwrap(),
        event.id.to_hex()
    );
}

#[tokio::test]
async fn an_earliest_wins_draft_is_resolved_before_any_re_signing() {
    let relay = MiniRelay::start().await;
    relay.set_drop_acks(1).await;
    let keys = Keys::generate();
    let me = keys.public_key();
    let publisher = open(&relay, keys, 30, 0).await;
    let now = publisher.now();
    let Outcome::Accepted(event) = publisher.publish(game_session(me, now)).await else {
        panic!()
    };
    // Found by the query: one frame, no second signature.
    assert_eq!(relay.received().await.len(), 1);
    assert_eq!(
        publisher.resolve(event.id).await,
        Resolution::Found(event.clone())
    );
    assert_eq!(publisher.resolve(id(9)).await, Resolution::ConfirmedAbsent);
}

#[tokio::test]
async fn a_challenge_without_acknowledgment_is_handed_back_unknown() {
    let relay = MiniRelay::start().await;
    relay.set_drop_acks(1).await;
    let keys = Keys::generate();
    let me = keys.public_key();
    let publisher = open(&relay, keys, 30, 0).await;
    let challenge = DirectChallenge {
        me,
        target: Keys::generate().public_key(),
        game: "sanki".to_owned(),
        rules: id(7),
        timing_relay: "wss://relay.sanki.app".to_owned(),
        time_control: vec![[Some(180), Some(2), None]],
        variant: "chess".to_owned(),
        accept_secs: std::num::NonZeroU64::new(120).unwrap(),
        not_after: None,
        content: String::new(),
    };
    let Outcome::Unknown(event) = publisher.publish(challenge).await else {
        panic!()
    };
    assert_eq!(relay.received().await.len(), 1);
    assert!(matches!(
        publisher.resolve(event.id).await,
        Resolution::Found(_)
    ));
    // The event reads back as a conforming challenge, mined at 0.
    let read = readers::direct_challenge(&event).unwrap();
    assert_eq!(read.accept_until, event.created_at.as_secs() + 120);
}

#[tokio::test]
async fn a_moot_conclusion_is_withheld_and_a_policy_is_replaceable() {
    let relay = MiniRelay::start().await;
    let keys = Keys::generate();
    let me = keys.public_key();
    let publisher = open(&relay, keys, 30, 0).await;
    let conclusion = Conclusion {
        session: id(1),
        first: me,
        second: Keys::generate().public_key(),
        verdict: Verdict {
            result: SeatResult {
                first: 100,
                second: 0,
            },
            status: "timeout".to_owned(),
        },
        window: Window::default(),
        still: Box::new(|_| false),
    };
    assert_eq!(
        publisher.publish(conclusion).await,
        Outcome::Withheld(Withheld::Moot)
    );
    let policy = ChallengePolicy {
        game: "sanki".to_owned(),
        policy: Policy::Everyone,
    };
    let Outcome::Accepted(event) = publisher.publish(policy).await else {
        panic!()
    };
    assert_eq!(
        readers::challenge_policy(&event).unwrap().mode,
        PolicyMode::Everyone
    );
}

#[tokio::test]
async fn the_lease_is_exclusive() {
    let relay = MiniRelay::start().await;
    let keys = Keys::generate();
    let again = Keys::parse(&keys.secret_key().to_secret_hex()).unwrap();
    let publisher = open(&relay, keys, 30, 0).await;
    let client = connected(&relay).await;
    let mut same = settings(&relay, 30, 0);
    same.data_dir = publisher.settings().data_dir.clone();
    let err = Publisher::open(client, again, same).await.unwrap_err();
    assert!(matches!(err, OpenError::KeyInUse(_)), "{err}");
}

#[tokio::test]
async fn a_silent_relay_proves_nothing() {
    let relay = MiniRelay::start().await;
    let keys = Keys::generate();
    let me = keys.public_key();
    let publisher = open(&relay, keys, 30, 0).await;
    let now = publisher.now();
    // The relay takes the Game Session without acknowledging it, and then
    // answers no query: the draft is neither re-signed nor believed absent.
    relay.set_drop_acks(1).await;
    relay.set_silent(true).await;
    let outcome = publisher.publish(game_session(me, now)).await;
    assert!(matches!(outcome, Outcome::Unknown(_)), "{outcome:?}");
    assert_eq!(relay.received().await.len(), 1, "no second signature");
    assert_eq!(publisher.resolve(id(9)).await, Resolution::Unknown);
    relay.set_silent(false).await;
    assert_eq!(publisher.resolve(id(9)).await, Resolution::ConfirmedAbsent);
}

#[tokio::test]
async fn an_unreachable_relay_does_not_open() {
    // Silent from the start: the round-trip probe gets no EOSE.
    let relay = MiniRelay::start().await;
    relay.set_silent(true).await;
    let client = connected(&relay).await;
    let err = Publisher::open(client, Keys::generate(), settings(&relay, 30, 0))
        .await
        .unwrap_err();
    assert!(matches!(err, OpenError::Unreachable), "{err}");
    // No relay at all: unreachable too.
    let none = Client::builder().build();
    let err = Publisher::open(none, Keys::generate(), settings(&relay, 30, 0))
        .await
        .unwrap_err();
    assert!(matches!(err, OpenError::Unreachable), "{err}");
}

#[tokio::test]
async fn a_closed_publisher_answers_closed() {
    let relay = MiniRelay::start().await;
    let publisher = open(&relay, Keys::generate(), 30, 0).await;
    let now = publisher.now();
    publisher.close();
    assert_eq!(publisher.publish(ply(1, now)).await, Outcome::Closed);
}
