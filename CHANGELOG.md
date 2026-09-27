# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **The client, extracted from `sanki-bot.rs`** (ADR-0045 §1, Plan step 2,
  first round). The bot's closed core moves here unchanged in behaviour, with
  its unit tests: `module` (the `wasmi` runtime and the Kernel ABI
  operations), `rules` (the two-hop load of a Rule System and its cache),
  `session` (the consumer's filters), `chain` (the live view), `clock`
  (`max_affordable`, was `clockmath`), `cadence`, `tags` and `publish` (the
  self-timed discipline and NIP-13 mining). The bot's persona-specific
  `publish_profile_abroad` stays with the bot.
- **`notation`** — the Ply content ↔ canonical PMN converters on
  `sashite_sanki_engine::pmn`: `to_pmn`, `to_content`, `content_of`,
  `history`. The chain's view now carries `contents` and `moves`, the
  selected chain in play order and in canonical PMN, computed in the
  positions the module's `apply` produced — the `moves` of an SEI `search`.
  The client no longer depends on `sashite-sanki-player`: the occurrence
  counts are keyed by the module's FEEN.
- **`relay`** — the relay's NIP-11 document (`RelayInfo`): the `created_at`
  window, the covered kinds, the proof-of-work minimum, and
  `check_self_timed`, which refuses a relay whose past bound is missing or
  above 5 s, or whose window leaves a session kind out.
- **`testing`** (feature) — the fleet's in-process NIP-01 relay, promoted,
  with the strict `created_at` window (`set_window`, the reference relay's
  wordings), a clock skew (`set_skew`) and the proof of work (`set_min_pow`)
  switchable; the reference module's native face as an `Oracle`
  (`module::native::Native`) and the signed suite events of
  `session::fixtures`, so a bot's tests drive the client without a `.wasm`.
- **`readers`** — typed readers of the suite's kinds (ADR-0045 §1): the
  Direct Challenge (`3420`, constraints 1–9 and the structural part of 10
  and 11), the founding reference of a Game Session (`3422`), the Elo
  Rating Attestation (`3426`), the Challenge Policy (`30420`), the profile
  (`0`, with its `bot` flag), the contact list (`3`), the mute list
  (`10000`), and `has_client_tag` for the NIP-89 tag. A reader returns the
  event typed, or a `NonConforming` naming the constraint it fails.
- **`drafts`** — the **sealed** `Publishable` trait and the drafts: a Ply,
  a Game Session, a Conclusion (with `still(stamp)`, `Moot` otherwise), an
  outgoing Direct Challenge (the mirror form, `accept_until` dated from the
  stamp), the profile (`bot: true` always), the contact list, the mute
  list, the Challenge Policy. Each type fixes its window, whether it is
  mined, its convergence (`Collapses`, `EarliestWins`, `Replaceable`,
  `NotIdempotent`) and whether it may take the governor's reserve. A draft
  is a function of its stamp: `Expired`, `Moot` or `Malformed` withhold it.
  A third party's draft does not compile (a `compile_fail` doctest).
- **`publisher`** — the Publisher of ADR-0045 §6: one queue ordered by
  `not_after`, a token governor over a 62 s window with the reserve of a
  tenth kept from standing events and outgoing challenges, stamps
  `max(floor(now) + 1, not_before)` on the relay's estimated clock,
  monotone per replaceable coordinate; at open, the lease on
  `<data_dir>/<pubkey>.lock` (`KeyInUse` otherwise), the relay's round trip,
  the mining benchmark and the latency bound; every event carries the
  `client` tag `sanki-bot` and, on the kinds that prescribe it, a `nonce`
  mined off the executor. Outcomes: `Accepted` (a `duplicate:` included),
  `Rejected` (classified `Stale`, `Future`, `Pow`, `RateLimited`,
  `Blocked`, `Invalid`; a timing rejection corrects the skew estimate and
  re-stamps once, `Pow` re-reads NIP-11 and re-mines once, `RateLimited`
  is logged at `error` and retried once), `Unknown` handled by convergence
  (a Ply resent unchanged while fresh, then re-stamped; an `EarliestWins`
  draft resolved by id on the relay itself and re-signed only when the
  relay answered that it is absent; a `NotIdempotent` draft handed back),
  `Withheld`, `Failed`, `Closed`. `resolve(id)` answers `Found`,
  `ConfirmedAbsent` or `Unknown` — a relay that does not answer proves
  nothing. Fifteen tests over the in-process relay.
- **`testing`** — three more faults on the mini relay: a per-signer rate
  limit with the reference wording, events stored but not acknowledged,
  and silence to every query.
- `chain::SessionView` carries the `clocks` the module computes at the
  tip (`None` when the session ended): what an SEI `clock` is built from.
- `Oracle` for `Box<O>`: a shared `Box<dyn Oracle + Send>` serves every
  call taking `&mut impl Oracle`.
- `session::fixtures::World` takes its `time_control` (default
  `["300", "3"]`): a short bank for a runtime's tests.
- **`query`** — a query the relay's EOSE proves: the notifications
  subscribed before the `REQ` is sent to the relay itself, the events
  collected (signatures verified, duplicates dropped) until the `EOSE` —
  or a `CLOSED` without a machine-readable prefix — for that subscription;
  a shutdown, an error `CLOSED` or the bound answer `None`. What the
  Publisher's `resolve` and a bot's reads at start rest on.
- `drafts::Replacing<D>` — a replaceable draft stamped after the relay's
  copy (`not_before = copy_at + 1`), so that the new event replaces a copy
  written by hand ahead of the relay's clock.
- `Publisher::open_leased` — `open` with a lease the caller took earlier
  (a bot takes it before its engine probe, ADR-0045 §7).
- `futures-util` is a plain dependency (the query's stream), no longer
  `testing`'s only.
- `Publisher::signed(id)` — whether this publisher signed the event (the
  newest 65,536 ids, resends and re-signings included): what a bot's echo
  detector asks. `Publisher::signer()` — the signer, for what it derives
  besides signatures (a bot's open seat and jitter), never its key.
- **`clock::check`** — the clock check of a bot's start (ADR-0045 §7,
  step 4): the module's `clock` primitive accepts exactly `max_affordable`
  and flags one second past it, on the boundary cases the unit test
  pinned; `ClockMismatch` names the case.

### Fixed

- **A disconnection mid-query was read as an answer.** `nostr-sdk` 0.45's
  `Relay::fetch_events` ends its stream silently when the relay
  disconnects while a `REQ` is open, and returns the events received so
  far as if the relay had said `EOSE`; `Publisher::resolve` read that as
  `ConfirmedAbsent`, and an `EarliestWins` draft could be signed twice.
  `resolve` now rests on `query`, which requires the `EOSE`.

- **A relay's rejection was read as an acceptance.** Since `nostr-sdk` 0.41,
  `Client::send_event` answers `Ok` whether or not any relay accepted the
  event; the rejection is in `Output::failed`, keyed by relay. The bot's
  `publish_self_timed` (on 0.44 since its first release) matched `Ok(_)` and
  returned the event as published, so a stale, future, under-mined,
  rate-limited or blocked event was believed accepted, and the `created_at`
  retry loop never ran. The client reads `success` and `failed`: an empty
  `success` is a rejection, classified from the relay's message. Tested
  against the in-process relay with its window on and a ±skew.
