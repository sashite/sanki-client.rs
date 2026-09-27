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

### Fixed

- **A relay's rejection was read as an acceptance.** Since `nostr-sdk` 0.41,
  `Client::send_event` answers `Ok` whether or not any relay accepted the
  event; the rejection is in `Output::failed`, keyed by relay. The bot's
  `publish_self_timed` (on 0.44 since its first release) matched `Ok(_)` and
  returned the event as published, so a stale, future, under-mined,
  rate-limited or blocked event was believed accepted, and the `created_at`
  retry loop never ran. The client reads `success` and `failed`: an empty
  `success` is a rejection, classified from the relay's message. Tested
  against the in-process relay with its window on and a ±skew.
