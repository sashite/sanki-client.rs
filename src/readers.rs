// SPDX-License-Identifier: Apache-2.0
//! Readers (ADR-0045 §1): a typed event, or the reason it does not conform.
//! Each reader checks what its NIP makes checkable from the event alone —
//! the structural and semantic constraints of its kind — and verifies the
//! signature; what needs another event (a `rules` reference resolving to a
//! Rule System, a rematch against its concluded session) is the caller's.
//!
//! | Kind | Reader |
//! |---|---|
//! | `3420` | [`direct_challenge`] |
//! | `3422` | [`founding_of`] (the founding reference; the terms are [`crate::session::terms`]) |
//! | `3426` | [`rating_attestation`] |
//! | `30420` | [`challenge_policy`] |
//! | `0` | [`profile`] |
//! | `3` | [`contacts`] |
//! | `10000` | [`mute_list`] |
//!
//! The Ply and the Conclusion are read for the module by
//! [`crate::session::ply`] and [`crate::session::conclusion`]; the Rule
//! System by [`crate::rules::accept_event`].

use std::fmt;

use nostr_sdk::prelude::*;
use serde_json::Value;

use crate::session::{self, Seat, Timing, KIND_DIRECT_CHALLENGE, KIND_GAME_SESSION, KIND_PAIRING};
use crate::tags;

/// The Elo Rating Attestation kind.
pub const KIND_RATING_ATTESTATION: u16 = 3426;
/// The Challenge Policy kind.
pub const KIND_CHALLENGE_POLICY: u16 = 30420;
/// The mute list kind (NIP-51).
pub const KIND_MUTE_LIST: u16 = 10000;

/// Why an event is not read: its kind, and the constraint it fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonConforming {
    /// The kind the reader expected.
    pub kind: u16,
    /// The constraint, in a few words.
    pub reason: &'static str,
}

impl NonConforming {
    const fn new(kind: u16, reason: &'static str) -> Self {
        Self { kind, reason }
    }
}

impl fmt::Display for NonConforming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a conforming kind {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for NonConforming {}

/// The kind and signature, first.
fn accept(event: &Event, kind: u16) -> Result<(), NonConforming> {
    if event.kind != Kind::Custom(kind) && event.kind.as_u16() != kind {
        return Err(NonConforming::new(kind, "another kind"));
    }
    if event.verify().is_err() {
        return Err(NonConforming::new(kind, "invalid signature"));
    }
    Ok(())
}

/// Whether the event carries the NIP-89 `client` tag naming `name` — the
/// tag every event this library publishes carries (ADR-0045 §2).
#[must_use]
pub fn has_client_tag(event: &Event, name: &str) -> bool {
    event.tags.iter().map(Tag::as_slice).any(|s| {
        s.first().map(String::as_str) == Some("client")
            && s.get(1).map(String::as_str) == Some(name)
    })
}

// ---- 3420 ----

/// The two references of a rematch challenge (kind `3420` §Rematch tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RematchRefs {
    /// The concluded Game Session.
    pub concluded: EventId,
    /// A Conclusion of it.
    pub concluded_by: EventId,
}

/// A Direct Challenge (kind `3420`), read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectChallenge {
    /// The event id.
    pub id: EventId,
    /// The challenger.
    pub challenger: PublicKey,
    /// The challenged player.
    pub opponent: PublicKey,
    /// `created_at`.
    pub created_at: u64,
    /// The game identifier.
    pub game: String,
    /// The Rule System event the session would be played under.
    pub rules: EventId,
    /// The timing designation.
    pub timing: Timing,
    /// The periods, in the ABI's encoding.
    pub time_control: Vec<[Option<u64>; 3]>,
    /// The `time_control` rows verbatim (identity comparisons).
    pub rows: Vec<Vec<String>>,
    /// The challenger's variant, when fixed.
    pub challenger_variant: Option<String>,
    /// The challenged player's variant, when fixed.
    pub opponent_variant: Option<String>,
    /// The seat the challenger claims, when declared.
    pub challenger_seat: Option<Seat>,
    /// `accept_until`.
    pub accept_until: u64,
    /// The rematch references, for a rematch challenge.
    pub rematch: Option<RematchRefs>,
    /// The free-form content.
    pub content: String,
}

impl DirectChallenge {
    /// Whether the challenge is a rematch challenge.
    #[must_use]
    pub const fn is_rematch(&self) -> bool {
        self.rematch.is_some()
    }
}

/// A content within kind `3420` §Content: at most 255 code points, free of
/// the forbidden control and bidirectional characters.
#[must_use]
pub fn challenge_content_ok(content: &str) -> bool {
    content.chars().count() <= 255
        && content.chars().all(|c| {
            !matches!(
                c,
                '\u{0000}'..='\u{0008}'
                    | '\u{000B}'
                    | '\u{000C}'
                    | '\u{000E}'..='\u{001F}'
                    | '\u{007F}'..='\u{009F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
            )
        })
}

/// The periods of a founding-family event, checked as kind `3420`
/// §Match-terms tags requires (`session::time_control_of` reads the shape;
/// the per-move rule on a `0` duration and a positive quota are here).
fn periods(event: &Event, kind: u16) -> Result<Vec<[Option<u64>; 3]>, NonConforming> {
    let periods = session::time_control_of(event)
        .map_err(|_| NonConforming::new(kind, "malformed time_control"))?;
    for [duration, increment, plies] in &periods {
        if plies.is_some_and(|p| p == 0) {
            return Err(NonConforming::new(kind, "a plies of 0"));
        }
        if duration == &Some(0) && (plies.is_none() || increment.is_none()) {
            return Err(NonConforming::new(
                kind,
                "a duration of 0 outside the per-move form",
            ));
        }
    }
    Ok(periods)
}

/// Reads a Direct Challenge: constraints 1–9 of kind `3420` §Semantic
/// constraints and the structural part of 10 and 11.
///
/// # Errors
///
/// The first constraint it fails.
pub fn direct_challenge(event: &Event) -> Result<DirectChallenge, NonConforming> {
    const K: u16 = KIND_DIRECT_CHALLENGE;
    accept(event, K)?;
    let challenger = event.pubkey;
    // Every p tag names a valid key and carries a role marker of this kind.
    let mut opponents = Vec::new();
    for tag in event.tags.iter().map(Tag::as_slice) {
        if tag.first().map(String::as_str) != Some("p") {
            continue;
        }
        let Some(pubkey) = tag.get(1).and_then(|hex| PublicKey::from_hex(hex).ok()) else {
            return Err(NonConforming::new(K, "malformed p tag"));
        };
        match tag.get(3).map(String::as_str) {
            Some("opponent") => opponents.push(pubkey),
            Some("timestamper") => {}
            _ => return Err(NonConforming::new(K, "a p tag without a role marker")),
        }
    }
    let [opponent] = opponents.as_slice() else {
        return Err(NonConforming::new(K, "not exactly one opponent"));
    };
    let opponent = *opponent;
    if opponent == challenger {
        return Err(NonConforming::new(K, "the opponent is the challenger"));
    }
    let timing = session::timing_of(event)
        .ok_or(NonConforming::new(K, "not exactly one timing designation"))?;
    match &timing {
        Timing::SelfTimed(relay) if !relay.starts_with("wss://") => {
            return Err(NonConforming::new(
                K,
                "the timing relay is not a wss:// URL",
            ));
        }
        Timing::Attested(timestamper) if *timestamper == challenger || *timestamper == opponent => {
            return Err(NonConforming::new(K, "the timestamper is a player"));
        }
        _ => {}
    }
    // Every p tag names someone other than the challenger.
    if event
        .tags
        .iter()
        .map(Tag::as_slice)
        .filter(|s| s.first().map(String::as_str) == Some("p"))
        .any(|s| s.get(1).map(String::as_str) == Some(&challenger.to_hex()))
    {
        return Err(NonConforming::new(K, "a p tag names the challenger"));
    }
    let game = session::exactly_one(event, "game")
        .filter(|g| session::is_identifier(g))
        .ok_or(NonConforming::new(K, "not exactly one valid game"))?
        .to_owned();
    let (challenger_variant, opponent_variant) = variants_of(event, K, &challenger, &opponent)?;
    let time_control = periods(event, K)?;
    let challenger_seat = match tags::values(event, "seat").as_slice() {
        [] => None,
        [seat] => Some(Seat::parse(seat).ok_or(NonConforming::new(K, "malformed seat"))?),
        _ => return Err(NonConforming::new(K, "several seat tags")),
    };
    let accept_until = session::exactly_one(event, "accept_until")
        .and_then(session::decimal)
        .ok_or(NonConforming::new(K, "not exactly one valid accept_until"))?;
    if accept_until <= event.created_at.as_secs() {
        return Err(NonConforming::new(
            K,
            "accept_until is not after created_at",
        ));
    }
    if !session::pow_ok(event) {
        return Err(NonConforming::new(K, "no honoured nonce"));
    }
    if !tags::values(event, "expiration").is_empty() {
        return Err(NonConforming::new(K, "an expiration tag"));
    }
    let rules = session::rules_ref(event)
        .ok_or(NonConforming::new(K, "not exactly one rules reference"))?;
    let rematch = match (
        tags::events_with_marker(event, "rematch_of").as_slice(),
        tags::events_with_marker(event, "concluded_by").as_slice(),
    ) {
        ([], []) => None,
        ([concluded], [concluded_by]) => Some(RematchRefs {
            concluded: *concluded,
            concluded_by: *concluded_by,
        }),
        _ => return Err(NonConforming::new(K, "malformed rematch references")),
    };
    let e_tags = if rematch.is_some() { 3 } else { 1 };
    if tags::count_named(event, "e") != e_tags {
        return Err(NonConforming::new(K, "an e tag besides the references"));
    }
    if rematch.is_some()
        && (challenger_variant.is_none() || opponent_variant.is_none() || challenger_seat.is_none())
    {
        return Err(NonConforming::new(
            K,
            "a rematch challenge leaves a term open",
        ));
    }
    if !challenge_content_ok(&event.content) {
        return Err(NonConforming::new(K, "content outside the constraints"));
    }
    Ok(DirectChallenge {
        id: event.id,
        challenger,
        opponent,
        created_at: event.created_at.as_secs(),
        game,
        rules,
        timing,
        time_control,
        rows: tags::time_control_rows(event),
        challenger_variant,
        opponent_variant,
        challenger_seat,
        accept_until,
        rematch,
        content: event.content.clone(),
    })
}

/// The variant tags of a founding-family event: at most one per player,
/// each a valid identifier, none for anyone else.
fn variants_of(
    event: &Event,
    kind: u16,
    a: &PublicKey,
    b: &PublicKey,
) -> Result<(Option<String>, Option<String>), NonConforming> {
    let mut for_a = None;
    let mut for_b = None;
    for tag in event.tags.iter().map(Tag::as_slice) {
        if tag.first().map(String::as_str) != Some("variant") {
            continue;
        }
        let (Some(pubkey), Some(variant), None) = (tag.get(1), tag.get(2), tag.get(3)) else {
            return Err(NonConforming::new(kind, "malformed variant tag"));
        };
        if !session::is_identifier(variant) {
            return Err(NonConforming::new(kind, "invalid variant identifier"));
        }
        let slot = if pubkey == &a.to_hex() {
            &mut for_a
        } else if pubkey == &b.to_hex() {
            &mut for_b
        } else {
            return Err(NonConforming::new(kind, "a variant for a third party"));
        };
        if slot.is_some() {
            return Err(NonConforming::new(kind, "two variants for one player"));
        }
        *slot = Some(variant.clone());
    }
    Ok((for_a, for_b))
}

// ---- 3422 ----

/// The founding a Game Session references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Founding {
    /// A Direct Challenge: the Game Session is the acceptance.
    DirectChallenge(EventId),
    /// A Pairing: the founding by one of its players.
    Pairing(EventId),
}

impl Founding {
    /// The founding event's id.
    #[must_use]
    pub const fn id(self) -> EventId {
        match self {
            Self::DirectChallenge(id) | Self::Pairing(id) => id,
        }
    }
}

/// The founding reference of a Game Session (kind `3422`): exactly one
/// `direct_challenge` or `pairing` marker. The terms are read against the
/// founding by [`crate::session::terms`].
///
/// # Errors
///
/// Not a Game Session, or no single founding reference.
pub fn founding_of(event: &Event) -> Result<Founding, NonConforming> {
    const K: u16 = KIND_GAME_SESSION;
    accept(event, K)?;
    let pairing = tags::events_with_marker(event, "pairing");
    let direct = tags::events_with_marker(event, "direct_challenge");
    match (pairing.as_slice(), direct.as_slice()) {
        ([id], []) => Ok(Founding::Pairing(*id)),
        ([], [id]) => Ok(Founding::DirectChallenge(*id)),
        _ => Err(NonConforming::new(K, "not exactly one founding reference")),
    }
}

/// The kind a founding is.
#[must_use]
pub const fn founding_kind(founding: Founding) -> u16 {
    match founding {
        Founding::DirectChallenge(_) => KIND_DIRECT_CHALLENGE,
        Founding::Pairing(_) => KIND_PAIRING,
    }
}

// ---- 3426 ----

/// An Elo Rating Attestation (kind `3426`), read.
#[derive(Debug, Clone, PartialEq)]
pub struct RatingAttestation {
    /// The rating authority.
    pub authority: PublicKey,
    /// The rated player.
    pub player: PublicKey,
    /// The duel: in this suite, the Game Session.
    pub duel: EventId,
    /// The game.
    pub game: String,
    /// The variants (0 to 2).
    pub variants: Vec<String>,
    /// The player's score in the duel, in `[0, 1]`.
    pub score: f64,
    /// The rating before.
    pub elo_pre: f64,
    /// The rating after.
    pub elo_post: f64,
    /// `created_at`.
    pub created_at: u64,
}

fn decimal_f64(text: &str) -> Option<f64> {
    if text.is_empty()
        || !text.chars().all(|c| c.is_ascii_digit() || c == '.')
        || text.starts_with('.')
        || text.ends_with('.')
        || text.matches('.').count() > 1
    {
        return None;
    }
    text.parse().ok().filter(|v: &f64| v.is_finite())
}

/// Reads a rating attestation: the references, the pool, the computation
/// tags in their domains.
///
/// # Errors
///
/// The first constraint it fails.
pub fn rating_attestation(event: &Event) -> Result<RatingAttestation, NonConforming> {
    const K: u16 = KIND_RATING_ATTESTATION;
    accept(event, K)?;
    let duel = match tags::events_with_marker(event, "duel").as_slice() {
        [id] => *id,
        _ => return Err(NonConforming::new(K, "not exactly one duel reference")),
    };
    let players: Vec<PublicKey> = event
        .tags
        .iter()
        .map(Tag::as_slice)
        .filter(|s| s.first().map(String::as_str) == Some("p"))
        .map(|s| {
            s.get(1)
                .and_then(|hex| PublicKey::from_hex(hex).ok())
                .ok_or(NonConforming::new(K, "malformed p tag"))
        })
        .collect::<Result<_, _>>()?;
    let [player] = players.as_slice() else {
        return Err(NonConforming::new(K, "not exactly one player"));
    };
    let game = session::exactly_one(event, "game")
        .filter(|g| session::is_identifier(g))
        .ok_or(NonConforming::new(K, "not exactly one valid game"))?
        .to_owned();
    let variants: Vec<String> = tags::values(event, "variant")
        .into_iter()
        .map(str::to_owned)
        .collect();
    if variants.len() > 2 || variants.iter().any(|v| !session::is_identifier(v)) {
        return Err(NonConforming::new(K, "invalid variant tags"));
    }
    let number = |name: &str| -> Result<f64, NonConforming> {
        session::exactly_one(event, name)
            .and_then(decimal_f64)
            .ok_or(NonConforming::new(K, "a rating tag is not a decimal"))
    };
    let score = number("score")?;
    if !(0.0..=1.0).contains(&score) {
        return Err(NonConforming::new(K, "score outside [0, 1]"));
    }
    let elo_pre = number("elo_pre")?;
    let elo_post = number("elo_post")?;
    let k_factor = number("k_factor")?;
    if k_factor <= 0.0 {
        return Err(NonConforming::new(K, "k_factor is not positive"));
    }
    Ok(RatingAttestation {
        authority: event.pubkey,
        player: *player,
        duel,
        game,
        variants,
        score,
        elo_pre,
        elo_post,
        created_at: event.created_at.as_secs(),
    })
}

// ---- 30420 ----

/// A Challenge Policy's mode (kind `30420`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    /// Everyone not muted.
    Everyone,
    /// The contact list, not muted.
    Following,
    /// Within `max_delta` of the user's rating, not muted.
    Rating,
    /// No one.
    Nobody,
}

impl PolicyMode {
    /// The mode `token` names.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "everyone" => Some(Self::Everyone),
            "following" => Some(Self::Following),
            "rating" => Some(Self::Rating),
            "nobody" => Some(Self::Nobody),
            _ => None,
        }
    }

    /// The token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Following => "following",
            Self::Rating => "rating",
            Self::Nobody => "nobody",
        }
    }
}

/// A Challenge Policy (kind `30420`), read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengePolicy {
    /// The signer.
    pub author: PublicKey,
    /// The scope: a game, or `*`.
    pub scope: String,
    /// The mode.
    pub mode: PolicyMode,
    /// `max_delta`, with `Rating`.
    pub max_delta: Option<u32>,
    /// `created_at`.
    pub created_at: u64,
}

/// Reads a Challenge Policy.
///
/// # Errors
///
/// The first constraint it fails.
pub fn challenge_policy(event: &Event) -> Result<ChallengePolicy, NonConforming> {
    const K: u16 = KIND_CHALLENGE_POLICY;
    accept(event, K)?;
    let scope = session::exactly_one(event, "d")
        .filter(|d| *d == "*" || session::is_identifier(d))
        .ok_or(NonConforming::new(K, "not exactly one valid d"))?
        .to_owned();
    let mode = session::exactly_one(event, "mode")
        .and_then(PolicyMode::parse)
        .ok_or(NonConforming::new(K, "not exactly one valid mode"))?;
    let deltas = tags::values(event, "max_delta");
    let max_delta = match (mode, deltas.as_slice()) {
        (PolicyMode::Rating, [delta]) => {
            let value = session::decimal(delta)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| (1..=1000).contains(v))
                .ok_or(NonConforming::new(K, "max_delta outside 1..=1000"))?;
            Some(value)
        }
        (PolicyMode::Rating, _) => {
            return Err(NonConforming::new(K, "rating without one max_delta"))
        }
        (_, []) => None,
        _ => return Err(NonConforming::new(K, "max_delta outside rating mode")),
    };
    if !event.content.is_empty() {
        return Err(NonConforming::new(K, "content is not empty"));
    }
    Ok(ChallengePolicy {
        author: event.pubkey,
        scope,
        mode,
        max_delta,
        created_at: event.created_at.as_secs(),
    })
}

// ---- 0, 3, 10000 ----

/// A profile (kind `0`), read.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The signer.
    pub author: PublicKey,
    /// NIP-24 `bot`, `true` only when the metadata says so.
    pub bot: bool,
    /// The whole metadata object.
    pub metadata: serde_json::Map<String, Value>,
    /// `created_at`.
    pub created_at: u64,
}

/// Reads a profile: a JSON object as content.
///
/// # Errors
///
/// Not a kind `0`, or a content that is not a JSON object.
pub fn profile(event: &Event) -> Result<Profile, NonConforming> {
    const K: u16 = 0;
    accept(event, K)?;
    let value: Value = serde_json::from_str(&event.content)
        .map_err(|_| NonConforming::new(K, "content is not JSON"))?;
    let Value::Object(metadata) = value else {
        return Err(NonConforming::new(K, "content is not a JSON object"));
    };
    Ok(Profile {
        author: event.pubkey,
        bot: metadata
            .get("bot")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        metadata,
        created_at: event.created_at.as_secs(),
    })
}

/// The public keys of every `p` tag, in tag order, duplicates kept.
fn p_tags(event: &Event, kind: u16) -> Result<Vec<PublicKey>, NonConforming> {
    event
        .tags
        .iter()
        .map(Tag::as_slice)
        .filter(|s| s.first().map(String::as_str) == Some("p"))
        .map(|s| {
            s.get(1)
                .and_then(|hex| PublicKey::from_hex(hex).ok())
                .ok_or(NonConforming::new(kind, "malformed p tag"))
        })
        .collect()
}

/// Reads a contact list (kind `3`, NIP-02): its `p` tags.
///
/// # Errors
///
/// Not a kind `3`, or a malformed `p` tag.
pub fn contacts(event: &Event) -> Result<Vec<PublicKey>, NonConforming> {
    accept(event, 3)?;
    p_tags(event, 3)
}

/// Reads a mute list (kind `10000`, NIP-51): its public `p` tags. The
/// encrypted content, when any, is not read: what a bot publishes is public.
///
/// # Errors
///
/// Not a kind `10000`, or a malformed `p` tag.
pub fn mute_list(event: &Event) -> Result<Vec<PublicKey>, NonConforming> {
    accept(event, KIND_MUTE_LIST)?;
    p_tags(event, KIND_MUTE_LIST)
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

    fn tag(values: &[&str]) -> Tag {
        Tag::parse(values.iter().copied()).unwrap()
    }

    fn challenge_tags(challenger: &Keys, opponent: &PublicKey) -> Vec<Tag> {
        let _ = challenger;
        vec![
            tag(&["p", &opponent.to_hex(), "", "opponent"]),
            tag(&["timing_relay", "wss://relay.sanki.app"]),
            tag(&["game", "sanki"]),
            tag(&["e", &"7".repeat(64), "", "rules"]),
            tag(&["time_control", "180", "2"]),
            tag(&["accept_until", "2000000300"]),
            tag(&["nonce", "0", "0"]),
        ]
    }

    fn signed(keys: &Keys, kind: u16, tags: Vec<Tag>, content: &str) -> Event {
        EventBuilder::new(Kind::Custom(kind), content)
            .tags(tags)
            .custom_created_at(Timestamp::from(1_700_000_000))
            .finalize(keys)
            .unwrap()
    }

    #[test]
    fn reads_a_fresh_direct_challenge() {
        let challenger = Keys::generate();
        let opponent = Keys::generate().public_key();
        let mut tags = challenge_tags(&challenger, &opponent);
        tags.push(tag(&[
            "variant",
            &challenger.public_key().to_hex(),
            "chess",
        ]));
        tags.push(tag(&["seat", "first"]));
        let event = signed(&challenger, 3420, tags, "hi");
        let c = direct_challenge(&event).unwrap();
        assert_eq!(c.opponent, opponent);
        assert_eq!(c.challenger_variant.as_deref(), Some("chess"));
        assert_eq!(c.opponent_variant, None);
        assert_eq!(c.challenger_seat, Some(Seat::First));
        assert_eq!(c.accept_until, 2_000_000_300);
        assert_eq!(c.time_control, vec![[Some(180), Some(2), None]]);
        assert_eq!(c.rows, vec![vec!["180", "2"]]);
        assert_eq!(
            c.timing,
            Timing::SelfTimed("wss://relay.sanki.app".to_owned())
        );
        assert!(!c.is_rematch());
    }

    #[test]
    fn refuses_what_the_constraints_forbid() {
        let challenger = Keys::generate();
        let opponent = Keys::generate().public_key();
        let base = challenge_tags(&challenger, &opponent);
        let cases: Vec<(Vec<Tag>, &str)> = vec![
            (
                base.iter()
                    .cloned()
                    .chain([tag(&[
                        "p",
                        &Keys::generate().public_key().to_hex(),
                        "",
                        "opponent",
                    ])])
                    .collect(),
                "not exactly one opponent",
            ),
            (
                base.iter()
                    .filter(|t| t.as_slice()[0] != "timing_relay")
                    .cloned()
                    .collect(),
                "not exactly one timing designation",
            ),
            (
                base.iter()
                    .cloned()
                    .map(|t| {
                        if t.as_slice()[0] == "timing_relay" {
                            tag(&["timing_relay", "ws://x"])
                        } else {
                            t
                        }
                    })
                    .collect(),
                "the timing relay is not a wss:// URL",
            ),
            (
                base.iter()
                    .cloned()
                    .map(|t| {
                        if t.as_slice()[0] == "game" {
                            tag(&["game", "Sanki"])
                        } else {
                            t
                        }
                    })
                    .collect(),
                "not exactly one valid game",
            ),
            (
                base.iter()
                    .cloned()
                    .map(|t| {
                        if t.as_slice()[0] == "time_control" {
                            tag(&["time_control", "0", "10"])
                        } else {
                            t
                        }
                    })
                    .collect(),
                "a duration of 0 outside the per-move form",
            ),
            (
                base.iter()
                    .cloned()
                    .map(|t| {
                        if t.as_slice()[0] == "accept_until" {
                            tag(&["accept_until", "1600000000"])
                        } else {
                            t
                        }
                    })
                    .collect(),
                "accept_until is not after created_at",
            ),
            (
                base.iter()
                    .filter(|t| t.as_slice()[0] != "nonce")
                    .cloned()
                    .collect(),
                "no honoured nonce",
            ),
            (
                base.iter()
                    .cloned()
                    .chain([tag(&["seat", "white"])])
                    .collect(),
                "malformed seat",
            ),
            (
                base.iter()
                    .cloned()
                    .chain([tag(&[
                        "variant",
                        &Keys::generate().public_key().to_hex(),
                        "chess",
                    ])])
                    .collect(),
                "a variant for a third party",
            ),
            (
                base.iter()
                    .cloned()
                    .chain([tag(&["e", &"8".repeat(64), "", "rematch_of"])])
                    .collect(),
                "malformed rematch references",
            ),
            (
                base.iter()
                    .cloned()
                    .chain([tag(&["expiration", "2000000400"])])
                    .collect(),
                "an expiration tag",
            ),
        ];
        for (tags, reason) in cases {
            let event = signed(&challenger, 3420, tags, "");
            assert_eq!(direct_challenge(&event).unwrap_err().reason, reason);
        }
        // Content constraints.
        let event = signed(&challenger, 3420, base.clone(), "\u{202E}");
        assert_eq!(
            direct_challenge(&event).unwrap_err().reason,
            "content outside the constraints"
        );
        // A self-challenge.
        let event = signed(
            &challenger,
            3420,
            challenge_tags(&challenger, &challenger.public_key()),
            "",
        );
        assert_eq!(
            direct_challenge(&event).unwrap_err().reason,
            "the opponent is the challenger"
        );
        // Another kind.
        let event = signed(&challenger, 3418, base, "");
        assert_eq!(direct_challenge(&event).unwrap_err().reason, "another kind");
    }

    #[test]
    fn reads_a_rematch_challenge_only_when_complete() {
        let challenger = Keys::generate();
        let opponent = Keys::generate().public_key();
        let mut tags = challenge_tags(&challenger, &opponent);
        tags.push(tag(&["e", &"8".repeat(64), "", "rematch_of"]));
        tags.push(tag(&["e", &"9".repeat(64), "", "concluded_by"]));
        let incomplete = signed(&challenger, 3420, tags.clone(), "");
        assert_eq!(
            direct_challenge(&incomplete).unwrap_err().reason,
            "a rematch challenge leaves a term open"
        );
        tags.push(tag(&[
            "variant",
            &challenger.public_key().to_hex(),
            "chess",
        ]));
        tags.push(tag(&["variant", &opponent.to_hex(), "ogi"]));
        tags.push(tag(&["seat", "second"]));
        let complete = signed(&challenger, 3420, tags, "");
        let c = direct_challenge(&complete).unwrap();
        assert!(c.is_rematch());
        assert_eq!(c.opponent_variant.as_deref(), Some("ogi"));
    }

    #[test]
    fn founding_reference_of_a_game_session() {
        let keys = Keys::generate();
        let direct = signed(
            &keys,
            3422,
            vec![tag(&["e", &"1".repeat(64), "", "direct_challenge"])],
            "",
        );
        assert_eq!(
            founding_of(&direct).unwrap(),
            Founding::DirectChallenge(EventId::from_hex(&"1".repeat(64)).unwrap())
        );
        let pairing = signed(
            &keys,
            3422,
            vec![tag(&["e", &"2".repeat(64), "", "pairing"])],
            "",
        );
        assert!(matches!(
            founding_of(&pairing).unwrap(),
            Founding::Pairing(_)
        ));
        assert_eq!(founding_kind(founding_of(&pairing).unwrap()), KIND_PAIRING);
        let both = signed(
            &keys,
            3422,
            vec![
                tag(&["e", &"1".repeat(64), "", "direct_challenge"]),
                tag(&["e", &"2".repeat(64), "", "pairing"]),
            ],
            "",
        );
        assert!(founding_of(&both).is_err());
    }

    #[test]
    fn reads_a_rating_attestation() {
        let authority = Keys::generate();
        let player = Keys::generate().public_key();
        let tags = vec![
            tag(&["e", &"1".repeat(64), "", "duel"]),
            tag(&["p", &player.to_hex()]),
            tag(&["game", "sanki"]),
            tag(&["variant", "chess"]),
            tag(&["score", "0.5"]),
            tag(&["elo_pre", "1500"]),
            tag(&["elo_post", "1504.5"]),
            tag(&["k_factor", "32"]),
        ];
        let event = signed(&authority, 3426, tags.clone(), "");
        let r = rating_attestation(&event).unwrap();
        assert_eq!(r.player, player);
        assert_eq!(r.score, 0.5);
        assert_eq!(r.elo_post, 1504.5);
        assert_eq!(r.variants, vec!["chess"]);
        let bad = signed(
            &authority,
            3426,
            tags.iter()
                .cloned()
                .map(|t| {
                    if t.as_slice()[0] == "score" {
                        tag(&["score", "1.5"])
                    } else {
                        t
                    }
                })
                .collect(),
            "",
        );
        assert_eq!(
            rating_attestation(&bad).unwrap_err().reason,
            "score outside [0, 1]"
        );
    }

    #[test]
    fn reads_a_challenge_policy() {
        let keys = Keys::generate();
        let policy = signed(
            &keys,
            30420,
            vec![
                tag(&["d", "sanki"]),
                tag(&["mode", "rating"]),
                tag(&["max_delta", "200"]),
            ],
            "",
        );
        let p = challenge_policy(&policy).unwrap();
        assert_eq!(p.mode, PolicyMode::Rating);
        assert_eq!(p.max_delta, Some(200));
        let wildcard = signed(
            &keys,
            30420,
            vec![tag(&["d", "*"]), tag(&["mode", "nobody"])],
            "",
        );
        assert_eq!(challenge_policy(&wildcard).unwrap().scope, "*");
        let stray = signed(
            &keys,
            30420,
            vec![
                tag(&["d", "sanki"]),
                tag(&["mode", "everyone"]),
                tag(&["max_delta", "200"]),
            ],
            "",
        );
        assert_eq!(
            challenge_policy(&stray).unwrap_err().reason,
            "max_delta outside rating mode"
        );
        let content = signed(
            &keys,
            30420,
            vec![tag(&["d", "sanki"]), tag(&["mode", "everyone"])],
            "x",
        );
        assert_eq!(
            challenge_policy(&content).unwrap_err().reason,
            "content is not empty"
        );
    }

    #[test]
    fn reads_profiles_and_lists() {
        let keys = Keys::generate();
        let bot = signed(&keys, 0, vec![], r#"{"name":"kitsune","bot":true}"#);
        assert!(profile(&bot).unwrap().bot);
        let person = signed(&keys, 0, vec![], r#"{"name":"cyril"}"#);
        assert!(!profile(&person).unwrap().bot);
        assert!(profile(&signed(&keys, 0, vec![], "[]")).is_err());
        let a = Keys::generate().public_key();
        let list = signed(
            &keys,
            3,
            vec![tag(&["p", &a.to_hex()]), tag(&["client", "sanki-bot"])],
            "",
        );
        assert_eq!(contacts(&list).unwrap(), vec![a]);
        assert!(has_client_tag(&list, "sanki-bot"));
        assert!(!has_client_tag(&list, "sanki.app"));
        let mutes = signed(&keys, 10000, vec![tag(&["p", &a.to_hex()])], "");
        assert_eq!(mute_list(&mutes).unwrap(), vec![a]);
        assert!(mute_list(&signed(&keys, 10000, vec![tag(&["p", "zz"])], "")).is_err());
    }
}
