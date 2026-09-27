// SPDX-License-Identifier: Apache-2.0
//! Drafts (ADR-0045 §1 *Drafts*): what a bot asks the [`crate::publisher`]
//! to publish. Each draft implements the **sealed** trait [`Publishable`],
//! and its type fixes three properties the bot cannot get wrong:
//!
//! - **its window**, in relay seconds: `not_before` and `not_after`;
//! - **its proof of work**: kinds `3420`, `3423` and `3425` carry exactly
//!   one `nonce` tag, mined to the relay's advertised minimum — at `0`, the
//!   tag is `["nonce","0","0"]`, since the kinds require it; kind `3422`
//!   and the standing events carry none;
//! - **its convergence** — what re-signing is allowed:
//!
//! | Convergence | Kinds | Re-signing is allowed… |
//! |---|---|---|
//! | `Collapses` | Ply | at any time within the window, with the same content: identical candidates are one |
//! | `EarliestWins` | Game Session, Conclusion | only once the previous signature is proven absent |
//! | `Replaceable` | `0`, `3`, `10000`, `30420` | at any time; the stamp is monotone per coordinate |
//! | `NotIdempotent` | Direct Challenge | never: a challenge whose outcome is `Unknown` is resolved by id before another is sent to the same target |
//!
//! A draft is a **function of its stamp**: [`Publishable::at`] gives the
//! tags and content for a `created_at`, or withholds the event — `Expired`
//! when the stamp is outside the window, `Moot` when the event no longer
//! means what it was drafted for (a Conclusion whose verdict the module no
//! longer yields at that stamp). A re-stamped claim can therefore never end
//! a game the bot did not decide to end.
//!
//! The Publisher adds what every event carries: the NIP-89 `client` tag and
//! the `nonce`.

use std::fmt;
use std::num::NonZeroU64;

use nostr_sdk::prelude::*;

use crate::module::Verdict;
use crate::readers::{Founding, PolicyMode, KIND_CHALLENGE_POLICY, KIND_MUTE_LIST};
use crate::session::{KIND_CONCLUSION, KIND_DIRECT_CHALLENGE, KIND_GAME_SESSION, KIND_PLY};

mod sealed {
    pub trait Sealed {}
}

/// A draft's window, in relay seconds (`created_at` bounds, inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Window {
    /// The earliest stamp.
    pub not_before: Option<u64>,
    /// The latest stamp.
    pub not_after: Option<u64>,
}

impl Window {
    /// Whether `stamp` is within the window.
    #[must_use]
    pub fn admits(&self, stamp: u64) -> bool {
        self.not_before.is_none_or(|b| stamp >= b) && self.not_after.is_none_or(|a| stamp <= a)
    }
}

/// How re-signing converges (see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convergence {
    /// Identical candidates are one.
    Collapses,
    /// The earliest signature wins; re-sign only once it is proven absent.
    EarliestWins,
    /// Replaceable by coordinate.
    Replaceable,
    /// Never re-signed blindly.
    NotIdempotent,
}

/// Why a draft yields no event at a stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withheld {
    /// The stamp is outside the window.
    Expired,
    /// The event would no longer mean what it was drafted for.
    Moot,
    /// The draft cannot be written as a conforming event (a content outside
    /// its kind's constraints, a period with a hole).
    Malformed,
}

impl fmt::Display for Withheld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expired => f.write_str("the window has closed"),
            Self::Moot => f.write_str("the event would no longer mean what it was drafted for"),
            Self::Malformed => f.write_str("the draft is not a conforming event"),
        }
    }
}

/// The tags and content of an event at a stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// The tags, without `client` and `nonce`.
    pub tags: Vec<Tag>,
    /// The content.
    pub content: String,
}

/// Something the Publisher publishes. Sealed: the drafts of this module
/// are the only ones, so that window, proof of work and convergence are
/// always the right ones for the kind (ADR-0045 §9: a third party's draft
/// must not compile).
///
/// ```compile_fail
/// use sashite_sanki_client::drafts::{Convergence, Parts, Publishable, Window, Withheld};
///
/// #[derive(Debug)]
/// struct Rogue;
///
/// impl Publishable for Rogue {
///     fn name(&self) -> &'static str { "rogue" }
///     fn kind(&self) -> nostr_sdk::prelude::Kind { nostr_sdk::prelude::Kind::Custom(3423) }
///     fn window(&self) -> Window { Window::default() }
///     fn mined(&self) -> bool { false }
///     fn convergence(&self) -> Convergence { Convergence::Collapses }
///     fn at(&self, _stamp: u64, _relay: &str) -> Result<Parts, Withheld> { Err(Withheld::Moot) }
/// }
/// ```
pub trait Publishable: sealed::Sealed + Send + Sync + fmt::Debug {
    /// A short name for logs.
    fn name(&self) -> &'static str;
    /// The kind.
    fn kind(&self) -> Kind;
    /// The window.
    fn window(&self) -> Window;
    /// Whether the kind prescribes a `nonce` tag.
    fn mined(&self) -> bool;
    /// The convergence.
    fn convergence(&self) -> Convergence;
    /// Whether the draft may take the last tokens of the rate governor
    /// (ADR-0045 §6: standing events and outgoing challenges may not).
    fn may_take_reserve(&self) -> bool {
        true
    }
    /// The event at `stamp`, with `relay` as the hint of every reference.
    ///
    /// # Errors
    ///
    /// [`Withheld`].
    fn at(&self, stamp: u64, relay: &str) -> Result<Parts, Withheld>;
}

fn e_marked(id: &EventId, relay: &str, marker: &str) -> Tag {
    Tag::custom("e", [id.to_hex(), relay.to_owned(), marker.to_owned()])
}

fn p_role(pubkey: &PublicKey, relay: &str, role: &str) -> Tag {
    Tag::custom("p", [pubkey.to_hex(), relay.to_owned(), role.to_owned()])
}

fn expired_unless(window: Window, stamp: u64) -> Result<(), Withheld> {
    if window.admits(stamp) {
        Ok(())
    } else {
        Err(Withheld::Expired)
    }
}

// ---- Ply ----

/// A Ply (kind `3423`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ply {
    /// The Game Session.
    pub session: EventId,
    /// The opponent.
    pub opponent: PublicKey,
    /// The step ordinal.
    pub step: u32,
    /// The content, the module's own.
    pub content: String,
    /// Whether the Ply offers a draw.
    pub draw: bool,
    /// The earliest stamp: the pace floor the bot fixed (§6), at or after
    /// the slot's anchor.
    pub not_before: u64,
    /// The latest stamp: the flag instant `F`.
    pub not_after: u64,
}

impl sealed::Sealed for Ply {}

impl Publishable for Ply {
    fn name(&self) -> &'static str {
        "ply"
    }
    fn kind(&self) -> Kind {
        Kind::Custom(KIND_PLY)
    }
    fn window(&self) -> Window {
        Window {
            not_before: Some(self.not_before),
            not_after: Some(self.not_after),
        }
    }
    fn mined(&self) -> bool {
        true
    }
    fn convergence(&self) -> Convergence {
        Convergence::Collapses
    }
    fn at(&self, stamp: u64, relay: &str) -> Result<Parts, Withheld> {
        expired_unless(self.window(), stamp)?;
        let mut tags = vec![
            e_marked(&self.session, relay, "game_session"),
            p_role(&self.opponent, relay, "opponent"),
            Tag::custom("step", [self.step.to_string()]),
        ];
        if self.draw {
            tags.push(Tag::custom("draw", Vec::<String>::new()));
        }
        Ok(Parts {
            tags,
            content: self.content.clone(),
        })
    }
}

// ---- Game Session ----

/// The terms a Game Session states (kind `3422` §Tags), resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPlan {
    /// The founding.
    pub founding: Founding,
    /// The game.
    pub game: String,
    /// The Rule System event, mirrored.
    pub rules: EventId,
    /// The timing relay, mirrored verbatim.
    pub timing_relay: String,
    /// The player seated `first`.
    pub first: PublicKey,
    /// The player seated `second`.
    pub second: PublicKey,
    /// The `first` player's variant.
    pub first_variant: String,
    /// The `second` player's variant.
    pub second_variant: String,
    /// The initial position the rule system prescribes (the content).
    pub position: String,
}

/// A Game Session (kind `3422`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameSession {
    /// The terms.
    pub plan: SessionPlan,
    /// The earliest stamp: the founding's `created_at`.
    pub not_before: u64,
    /// The latest stamp: the challenge's `accept_until`, or the Pairing's
    /// `found_until`.
    pub not_after: u64,
}

impl sealed::Sealed for GameSession {}

impl Publishable for GameSession {
    fn name(&self) -> &'static str {
        "game session"
    }
    fn kind(&self) -> Kind {
        Kind::Custom(KIND_GAME_SESSION)
    }
    fn window(&self) -> Window {
        Window {
            not_before: Some(self.not_before),
            not_after: Some(self.not_after),
        }
    }
    fn mined(&self) -> bool {
        false
    }
    fn convergence(&self) -> Convergence {
        Convergence::EarliestWins
    }
    fn at(&self, stamp: u64, relay: &str) -> Result<Parts, Withheld> {
        expired_unless(self.window(), stamp)?;
        let p = &self.plan;
        let marker = match p.founding {
            Founding::DirectChallenge(_) => "direct_challenge",
            Founding::Pairing(_) => "pairing",
        };
        Ok(Parts {
            tags: vec![
                e_marked(&p.founding.id(), relay, marker),
                Tag::custom("timing_relay", [p.timing_relay.clone()]),
                Tag::custom("game", [p.game.clone()]),
                e_marked(&p.rules, relay, "rules"),
                p_role(&p.first, relay, "player"),
                p_role(&p.second, relay, "player"),
                Tag::custom("seat", [p.first.to_hex(), "first".to_owned()]),
                Tag::custom("seat", [p.second.to_hex(), "second".to_owned()]),
                Tag::custom("variant", [p.first.to_hex(), p.first_variant.clone()]),
                Tag::custom("variant", [p.second.to_hex(), p.second_variant.clone()]),
            ],
            content: p.position.clone(),
        })
    }
}

// ---- Conclusion ----

/// A Conclusion (kind `3425`): a claim of `verdict`, published only while
/// `still` holds at the stamp — the bot's re-check of `verdict_at(me,
/// stamp)` (ADR-0045 §1: `Moot` otherwise).
pub struct Conclusion {
    /// The Game Session.
    pub session: EventId,
    /// The player seated `first`.
    pub first: PublicKey,
    /// The player seated `second`.
    pub second: PublicKey,
    /// The verdict claimed.
    pub verdict: Verdict,
    /// The window (a draw acceptance: the decision instant to the bot's
    /// own flag; other Conclusions: none).
    pub window: Window,
    /// Whether the verdict is still the one the module yields at a stamp.
    pub still: Box<dyn Fn(u64) -> bool + Send + Sync>,
}

impl fmt::Debug for Conclusion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conclusion")
            .field("session", &self.session)
            .field("verdict", &self.verdict)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

impl sealed::Sealed for Conclusion {}

impl Publishable for Conclusion {
    fn name(&self) -> &'static str {
        "conclusion"
    }
    fn kind(&self) -> Kind {
        Kind::Custom(KIND_CONCLUSION)
    }
    fn window(&self) -> Window {
        self.window
    }
    fn mined(&self) -> bool {
        true
    }
    fn convergence(&self) -> Convergence {
        Convergence::EarliestWins
    }
    fn at(&self, stamp: u64, relay: &str) -> Result<Parts, Withheld> {
        expired_unless(self.window, stamp)?;
        if !(self.still)(stamp) {
            return Err(Withheld::Moot);
        }
        Ok(Parts {
            tags: vec![
                e_marked(&self.session, relay, "game_session"),
                p_role(&self.first, relay, "player"),
                p_role(&self.second, relay, "player"),
                Tag::custom("seat", [self.first.to_hex(), "first".to_owned()]),
                Tag::custom("seat", [self.second.to_hex(), "second".to_owned()]),
                Tag::custom(
                    "result",
                    [self.first.to_hex(), self.verdict.result.first.to_string()],
                ),
                Tag::custom(
                    "result",
                    [self.second.to_hex(), self.verdict.result.second.to_string()],
                ),
            ],
            content: self.verdict.status.clone(),
        })
    }
}

// ---- Direct Challenge ----

/// An outgoing Direct Challenge (kind `3420`), in the mirror form: both
/// variants fixed to `variant` (ADR-0045 §5 *Sending one*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectChallenge {
    /// The challenger (the bot).
    pub me: PublicKey,
    /// The target.
    pub target: PublicKey,
    /// The game.
    pub game: String,
    /// The Rule System event.
    pub rules: EventId,
    /// The timing relay.
    pub timing_relay: String,
    /// The periods, as kind `3420` writes them.
    pub time_control: Vec<[Option<u64>; 3]>,
    /// The variant, for both players.
    pub variant: String,
    /// `accept_until = created_at + accept_secs`; positive, as kind `3420`
    /// requires `accept_until` after `created_at`.
    pub accept_secs: NonZeroU64,
    /// The latest stamp (its rank in the queue, §6), when bounded.
    pub not_after: Option<u64>,
    /// The content.
    pub content: String,
}

impl sealed::Sealed for DirectChallenge {}

impl Publishable for DirectChallenge {
    fn name(&self) -> &'static str {
        "direct challenge"
    }
    fn kind(&self) -> Kind {
        Kind::Custom(KIND_DIRECT_CHALLENGE)
    }
    fn window(&self) -> Window {
        Window {
            not_before: None,
            not_after: self.not_after,
        }
    }
    fn mined(&self) -> bool {
        true
    }
    fn convergence(&self) -> Convergence {
        Convergence::NotIdempotent
    }
    fn may_take_reserve(&self) -> bool {
        false
    }
    fn at(&self, stamp: u64, relay: &str) -> Result<Parts, Withheld> {
        expired_unless(self.window(), stamp)?;
        let mut tags = vec![
            p_role(&self.target, relay, "opponent"),
            Tag::custom("timing_relay", [self.timing_relay.clone()]),
            Tag::custom("game", [self.game.clone()]),
            e_marked(&self.rules, relay, "rules"),
            Tag::custom("variant", [self.me.to_hex(), self.variant.clone()]),
            Tag::custom("variant", [self.target.to_hex(), self.variant.clone()]),
        ];
        if self.time_control.is_empty() || !crate::readers::challenge_content_ok(&self.content) {
            return Err(Withheld::Malformed);
        }
        for period in &self.time_control {
            // Positional: a hole would shift the elements' meaning.
            let values: Vec<String> = match period {
                [Some(d), None, None] if *d > 0 => vec![d.to_string()],
                [Some(d), Some(i), None] if *d > 0 => vec![d.to_string(), i.to_string()],
                [Some(d), Some(i), Some(p)] if *p > 0 => {
                    vec![d.to_string(), i.to_string(), p.to_string()]
                }
                _ => return Err(Withheld::Malformed),
            };
            tags.push(Tag::custom("time_control", values));
        }
        tags.push(Tag::custom(
            "accept_until",
            [stamp.saturating_add(self.accept_secs.get()).to_string()],
        ));
        Ok(Parts {
            tags,
            content: self.content.clone(),
        })
    }
}

// ---- standing events ----

/// The profile (kind `0`): the metadata, with `bot: true` always written.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The metadata; `bot` is set to `true` whatever it says.
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl Profile {
    /// The content, with `bot: true`.
    #[must_use]
    pub fn content(&self) -> String {
        let mut metadata = self.metadata.clone();
        metadata.insert("bot".to_owned(), serde_json::Value::Bool(true));
        serde_json::Value::Object(metadata).to_string()
    }
}

impl sealed::Sealed for Profile {}

impl Publishable for Profile {
    fn name(&self) -> &'static str {
        "profile"
    }
    fn kind(&self) -> Kind {
        Kind::Metadata
    }
    fn window(&self) -> Window {
        Window::default()
    }
    fn mined(&self) -> bool {
        false
    }
    fn convergence(&self) -> Convergence {
        Convergence::Replaceable
    }
    fn may_take_reserve(&self) -> bool {
        false
    }
    fn at(&self, _stamp: u64, _relay: &str) -> Result<Parts, Withheld> {
        Ok(Parts {
            tags: Vec::new(),
            content: self.content(),
        })
    }
}

/// The contact list (kind `3`), equal to the configured `follows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contacts(pub Vec<PublicKey>);

impl sealed::Sealed for Contacts {}

impl Publishable for Contacts {
    fn name(&self) -> &'static str {
        "contact list"
    }
    fn kind(&self) -> Kind {
        Kind::ContactList
    }
    fn window(&self) -> Window {
        Window::default()
    }
    fn mined(&self) -> bool {
        false
    }
    fn convergence(&self) -> Convergence {
        Convergence::Replaceable
    }
    fn may_take_reserve(&self) -> bool {
        false
    }
    fn at(&self, _stamp: u64, relay: &str) -> Result<Parts, Withheld> {
        Ok(Parts {
            tags: self
                .0
                .iter()
                .map(|p| Tag::custom("p", [p.to_hex(), relay.to_owned()]))
                .collect(),
            content: String::new(),
        })
    }
}

/// The mute list (kind `10000`), equal to the configured `blocks`, public.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuteList(pub Vec<PublicKey>);

impl sealed::Sealed for MuteList {}

impl Publishable for MuteList {
    fn name(&self) -> &'static str {
        "mute list"
    }
    fn kind(&self) -> Kind {
        Kind::Custom(KIND_MUTE_LIST)
    }
    fn window(&self) -> Window {
        Window::default()
    }
    fn mined(&self) -> bool {
        false
    }
    fn convergence(&self) -> Convergence {
        Convergence::Replaceable
    }
    fn may_take_reserve(&self) -> bool {
        false
    }
    fn at(&self, _stamp: u64, _relay: &str) -> Result<Parts, Withheld> {
        Ok(Parts {
            tags: self
                .0
                .iter()
                .map(|p| Tag::custom("p", [p.to_hex()]))
                .collect(),
            content: String::new(),
        })
    }
}

/// A policy to publish (kind `30420`): the mode, with what it requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Everyone.
    Everyone,
    /// The contact list.
    Following,
    /// Within `max_delta` (1 to 1,000).
    Rating {
        /// The largest rating difference admitted.
        max_delta: u32,
    },
    /// No one.
    Nobody,
}

impl Policy {
    /// The mode.
    #[must_use]
    pub const fn mode(self) -> PolicyMode {
        match self {
            Self::Everyone => PolicyMode::Everyone,
            Self::Following => PolicyMode::Following,
            Self::Rating { .. } => PolicyMode::Rating,
            Self::Nobody => PolicyMode::Nobody,
        }
    }
}

/// The Challenge Policy (kind `30420`, `d = game`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengePolicy {
    /// The game.
    pub game: String,
    /// The policy.
    pub policy: Policy,
}

impl sealed::Sealed for ChallengePolicy {}

impl Publishable for ChallengePolicy {
    fn name(&self) -> &'static str {
        "challenge policy"
    }
    fn kind(&self) -> Kind {
        Kind::Custom(KIND_CHALLENGE_POLICY)
    }
    fn window(&self) -> Window {
        Window::default()
    }
    fn mined(&self) -> bool {
        false
    }
    fn convergence(&self) -> Convergence {
        Convergence::Replaceable
    }
    fn may_take_reserve(&self) -> bool {
        false
    }
    fn at(&self, _stamp: u64, _relay: &str) -> Result<Parts, Withheld> {
        let mut tags = vec![
            Tag::identifier(self.game.clone()),
            Tag::custom("mode", [self.policy.mode().token()]),
        ];
        if let Policy::Rating { max_delta } = self.policy {
            if !(1..=1000).contains(&max_delta) {
                return Err(Withheld::Malformed);
            }
            tags.push(Tag::custom("max_delta", [max_delta.to_string()]));
        }
        Ok(Parts {
            tags,
            content: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use crate::module::SeatResult;

    fn id(byte: u8) -> EventId {
        EventId::from_slice(&[byte; 32]).unwrap()
    }

    #[test]
    fn a_ply_within_its_window() {
        let ply = Ply {
            session: id(1),
            opponent: Keys::generate().public_key(),
            step: 3,
            content: r#"["e2","e4",null]"#.to_owned(),
            draw: true,
            not_before: 100,
            not_after: 200,
        };
        assert_eq!(ply.at(99, "wss://r"), Err(Withheld::Expired));
        assert_eq!(ply.at(201, "wss://r"), Err(Withheld::Expired));
        let parts = ply.at(150, "wss://r").unwrap();
        assert_eq!(parts.tags.len(), 4);
        assert_eq!(parts.tags[2].as_slice(), &["step", "3"]);
        assert_eq!(parts.tags[3].as_slice(), &["draw"]);
        assert!(ply.mined());
        assert_eq!(ply.convergence(), Convergence::Collapses);
        assert!(ply.may_take_reserve());
    }

    #[test]
    fn a_conclusion_is_moot_when_the_verdict_moved() {
        let conclusion = Conclusion {
            session: id(1),
            first: Keys::generate().public_key(),
            second: Keys::generate().public_key(),
            verdict: Verdict {
                result: SeatResult {
                    first: 100,
                    second: 0,
                },
                status: "timeout".to_owned(),
            },
            window: Window::default(),
            still: Box::new(|stamp| stamp < 1_000),
        };
        let parts = conclusion.at(999, "wss://r").unwrap();
        assert_eq!(parts.content, "timeout");
        assert_eq!(parts.tags[5].as_slice()[2], "100");
        assert_eq!(conclusion.at(1_000, "wss://r"), Err(Withheld::Moot));
        assert_eq!(conclusion.convergence(), Convergence::EarliestWins);
    }

    #[test]
    fn a_direct_challenge_dates_its_acceptance_from_the_stamp() {
        let me = Keys::generate().public_key();
        let challenge = DirectChallenge {
            me,
            target: Keys::generate().public_key(),
            game: "sanki".to_owned(),
            rules: id(7),
            timing_relay: "wss://relay.sanki.app".to_owned(),
            time_control: vec![[Some(180), Some(2), None], [Some(0), Some(10), Some(1)]],
            variant: "chess".to_owned(),
            accept_secs: NonZeroU64::new(120).unwrap(),
            not_after: Some(500),
            content: String::new(),
        };
        let parts = challenge.at(400, "wss://relay.sanki.app").unwrap();
        let accept_until = parts
            .tags
            .iter()
            .find(|t| t.as_slice()[0] == "accept_until")
            .unwrap();
        assert_eq!(accept_until.as_slice()[1], "520");
        let periods: Vec<&Tag> = parts
            .tags
            .iter()
            .filter(|t| t.as_slice()[0] == "time_control")
            .collect();
        assert_eq!(periods[0].as_slice(), &["time_control", "180", "2"]);
        assert_eq!(periods[1].as_slice(), &["time_control", "0", "10", "1"]);
        assert_eq!(challenge.at(501, "wss://r"), Err(Withheld::Expired));
        assert!(!challenge.may_take_reserve());
        assert_eq!(challenge.convergence(), Convergence::NotIdempotent);
        // A period with a hole, or a content outside the constraints, is
        // no event.
        let mut hole = challenge.clone();
        hole.time_control = vec![[Some(180), None, Some(40)]];
        assert_eq!(hole.at(400, "wss://r"), Err(Withheld::Malformed));
        let mut loud = challenge.clone();
        loud.content = "\u{202E}".to_owned();
        assert_eq!(loud.at(400, "wss://r"), Err(Withheld::Malformed));
    }

    #[test]
    fn the_profile_is_always_a_bot() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("name".to_owned(), serde_json::Value::from("kitsune"));
        metadata.insert("bot".to_owned(), serde_json::Value::Bool(false));
        let profile = Profile { metadata };
        let content = profile.at(1, "").unwrap().content;
        assert!(content.contains(r#""bot":true"#));
        assert_eq!(profile.kind(), Kind::Metadata);
        assert_eq!(profile.window(), Window::default());
    }

    #[test]
    fn the_policy_writes_max_delta_only_for_rating() {
        let rating = ChallengePolicy {
            game: "sanki".to_owned(),
            policy: Policy::Rating { max_delta: 200 },
        };
        let parts = rating.at(1, "").unwrap();
        assert_eq!(parts.tags.len(), 3);
        let everyone = ChallengePolicy {
            game: "sanki".to_owned(),
            policy: Policy::Everyone,
        };
        assert_eq!(everyone.at(1, "").unwrap().tags.len(), 2);
        let wild = ChallengePolicy {
            game: "sanki".to_owned(),
            policy: Policy::Rating { max_delta: 5000 },
        };
        assert_eq!(wild.at(1, ""), Err(Withheld::Malformed));
    }
}
