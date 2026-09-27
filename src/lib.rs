// SPDX-License-Identifier: Apache-2.0
//! The Sanki protocol client for bots (ADR-0045 §1): **the verbs, with no
//! policy and no autonomy**. A bot — `sashite-sanki-bot`, or one of its own —
//! decides; this crate makes the decision conform.
//!
//! | Module | What it does |
//! |---|---|
//! | [`rules`] | loads the **Rule System** (kind `3417`) and its module in two verifiable hops, caches both, instantiates the module |
//! | [`module`] | runs the module under `wasmi` as the only rules [`module::Oracle`]: `describe`, `legal_moves`, `apply`, `natural_state`, `verdict_at`, `check`, `select_conclusion` |
//! | [`session`] | assembles a session for the module — the consumer's filters of *Kernel ABI — Sanki*: the terms of a Game Session against its founding, the conforming Plies, Conclusions and attestations |
//! | [`chain`] | the live view of a session at an instant: the selected chain, the tip, whose turn, the clocks, the predicted verdict — and the chain in canonical PMN, ready for an SEI `search` |
//! | [`notation`] | the Ply content ↔ canonical PMN converters, on `sashite_sanki_engine::pmn` (the oracle of notation only) |
//! | [`clock`] | how many seconds a mover may still take (`max_affordable`) |
//! | [`cadence`] | the cadence family of a time control (*Cadence — Sanki*) |
//! | [`readers`] | typed readers of the suite's kinds (`3420`, `3422`, `3426`, `30420`, `0`, `3`, `10000`): the event, or the reason it does not conform |
//! | [`drafts`] | the sealed drafts: a Ply, a Game Session, a Conclusion, a Direct Challenge, the standing events — each with its window, its proof of work and its convergence |
//! | [`publisher`] | one queue ordered by deadline, the token governor with its reserve, stamping without backdating, the outcomes and what each convergence does with `Unknown`, the lease |
//! | [`query`] | a query the relay's EOSE proves: `Some(events)`, or `None` when nothing is proven |
//! | [`relay`] | the relay's NIP-11 document, read as a self-timed client must |
//! | [`publish`] | the primitives: the relay clock estimate, NIP-13 mining, `publish_self_timed` for a caller without the Publisher |
//! | [`tags`] | readers of the suite's tag conventions (roles, markers, rows) |
//! | [`testing`] | an in-process NIP-01 relay with the strict window and the proof of work switchable (`testing` feature) |
//!
//! **The oracle.** Everything the rules decide — a move's legality, the
//! canonical chain, the clocks, a verdict — is asked of the session's module,
//! never recomputed here (ADR-0034). The native rules crate serves one
//! purpose: writing and reading the canonical PMN of *SEI Rules Document —
//! Sanki*, in positions the module produced.
//!
//! **What this crate is not.** It sends no challenge, founds no session, plays
//! no move and claims no verdict on its own; it holds no key beyond the
//! signing a caller asks for (a signer the caller moves into the
//! [`publisher::Publisher`]); it has no configuration.

pub mod cadence;
pub mod chain;
pub mod clock;
pub mod drafts;
pub mod module;
pub mod notation;
pub mod publish;
pub mod publisher;
pub mod query;
pub mod readers;
pub mod relay;
pub mod rules;
pub mod session;
pub mod tags;
#[cfg(feature = "testing")]
pub mod testing;
