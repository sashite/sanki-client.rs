# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] — 2026-09-28

The verbs of the pool and of the profile abroad (ADR-0047 §Plan step 1),
for a bot that enters the matchmaking pool and is found on the public
relays. Nothing of 0.1 changes.

### Added

- **`readers::open_challenge`** (kind `3418`): the nine semantic
  constraints of the kind, the absence of an NIP-40 `expiration` tag and
  of any `e` tag but the `rules` reference; the `filter` in its three
  forms (`Filter`, `RatingScope`), the role-keyed variants, the periods
  and their rows verbatim.
- **`readers::pairing`** (kind `3419`), read **against its two Open
  Challenges** in either order: the consent constraints 1 to 9 and 11 to
  15 — the signer the matchmaker of both, the references, the players,
  the seats, the timing designation identical to both entries', the game,
  the variants respecting every role term, the time control identical in
  presence and value, the rules, `created_at` within both `accept_until`,
  the matchmaker no player, an empty content, `found_until`, no arbiter.
  Constraint 10 (the filters, external data) is the caller's.
- **`readers::relay_list`** (kind `10002`): the relays of the `r` tags.
- **`drafts::OpenChallenge`**: the mirror entry — both roles fixed to one
  variant, the courted entry's periods, the bot's own filter, an
  **absolute** `accept_until` and a window that never reaches it; mined,
  outside the reserve, `NotIdempotent` (two live entries are two
  Pairings). **`drafts::RelayList`** (kind `10002`), a standing event.
- **`relay::POOL_KINDS`** (`3418`, `3419`): what a bot entering the pool
  checks the relay's window for, and watches for echoes.
- **`testing::pairing_of`**: a scripted matchmaker's Pairing of two
  entries — conforming, or not, as the test asks — for the bot's tests.
- `session::KIND_OPEN_CHALLENGE`; `tags::hex_id` and `tags::hex_pubkey`.

### Changed

- **An id or a pubkey is read in the suite's one form**, 64 lowercase hex
  characters: a `rules` reference, a `p` tag's key, a `seat` or `variant`
  keyed by pubkey written as `note1…`, `npub1…` or uppercase hex names
  nothing — the readers refuse what the kinds' wording (kind `3420`
  constraint 10, kind `3419` §Match-terms tags) forbids, where
  `nostr-sdk`'s parsers admitted it.
- `readers::pairing` compares `created_at` to the entries' `accept_until`
  in self-timed mode only; in attested mode the canonical timing is the
  attestation's, the caller's to compare.

## [0.1.2] — 2026-09-28

### Fixed

- **Nothing is signed or sent while the relay is away.** `nostr-sdk`
  queues a message sent to a relay that is reconnecting and flushes the
  queue once the connection is back: an event signed during a network cut
  reached the relay seconds later, stale — rejected at best, or accepted
  beside its re-stamped copy. The `Publisher`'s dispatcher now holds the
  queue while the client's relay is not connected: a draft waits there for
  a fresh stamp at the reconnection, or expires there (`Withheld(Expired)`)
  — no signature, no mining, no token meanwhile; a loss between the choice
  and the send puts the draft back the same way. *Away* is as the client
  knows it: a socket closed by the peer, or a pong missed (`nostr-sdk`
  pings every 55 s); a silent cut is seen only then.

### Added

- `testing::MiniRelay::cut` and `restore` — every connection closed and
  every new one refused until the restore, the store kept: a network cut;
  cut and restored at once, a relay restart.

## [0.1.1] — 2026-09-28

### Added

- **The relay's clock, learnt exactly.** A timing rejection of the
  reference relay states the relay's clock (`… (relay clock M, tolerance
  Ts)`): the skew becomes `M − host`, `host` read as the event was sent —
  `RelayClock::learn`, `publish::relay_clock_in` — instead of a two-second
  step per rejection; the step remains for a relay that states nothing.
  A stamp forced above the window by a draft's `not_before` waits for the
  corrected clock rather than failing. The same on the resend of an
  unacknowledged Ply that meets a timing verdict. A ±10 s skew now costs
  one rejection, never a lost step.
- `testing::MiniRelay::set_store_delay` — an accepted event kept in
  transit for a while (stored, acknowledged and delivered after the
  delay): a client's death with a Ply on the wire; `rejected()` — every
  rejection answered, with its wording. The relay stores an event once
  (a delayed frame followed by its resend).

## [0.1.0] — 2026-09-28

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
