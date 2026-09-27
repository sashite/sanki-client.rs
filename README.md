# sanki-client

The [Sanki](https://sashite.com/) protocol client for bots — **the verbs, with
no policy and no autonomy** (ADR-0045 §1). A bot decides; this crate makes
the decision conform to the suite's NIPs and to the session's rule system.

## The stack, in one table

| Concept | Where |
|---|---|
| The engine protocol | [SEI](https://sashite.dev/specs/sei/1.0.0/) — how a host and an engine talk |
| What SEI means for Sanki | *SEI Rules Document — Sanki*: canonical FEEN and PMN, styles `W` `J` `C`, the nine pairings |
| The rules | [`sashite-sanki-engine`](https://github.com/sashite/sanki-engine.rs) (native, with `pmn`) and [`sashite-sanki-kernel-wasm`](https://github.com/sashite/sanki-kernel-wasm.rs) (the module a Rule System names) |
| **This client** | the protocol: the rule system loaded and run, the session assembled and read, notation, the relay's information, self-timed publishing |
| The bot that hosts an engine on Nostr | [`sanki-bot.rs`](https://github.com/sashite/sanki-bot.rs): challenges, sessions, Plies, clocks, Conclusions — on this client |
| The engine to fork | [`sanki-sei-random-engine.rs`](https://github.com/sashite/sanki-sei-random-engine.rs): a complete SEI engine that plays at random |

## What it does

| Module | What it does |
|---|---|
| `rules` | loads the **Rule System** (kind `3417`) and its module in two verifiable hops — the event by id, the module by digest — caches both on disk, instantiates the module and checks what it reports against the event |
| `module` | runs the module under `wasmi` as the only rules oracle: `describe`, `legal_moves`, `apply`, `natural_state`, `verdict_at`, `check`, `select_conclusion` |
| `session` | assembles a session for the module — the consumer's filters of *Kernel ABI — Sanki*: the terms of a Game Session against its founding, the conforming Plies, Conclusions and attestations |
| `chain` | the live view of a session at an instant: the selected chain, the tip, whose turn, the clocks, the predicted verdict — and the chain in canonical PMN, ready for an SEI `search` |
| `notation` | the Ply content ↔ canonical PMN converters, on `sashite_sanki_engine::pmn` |
| `clock`, `cadence` | how many seconds a mover may still take; the cadence family of a time control |
| `readers` | typed readers of the suite's kinds — the Direct Challenge, the Game Session's founding, the rating attestation, the Challenge Policy, the profile and the lists — or the reason an event does not conform |
| `drafts` | the sealed drafts a bot publishes: a Ply, a Game Session, a Conclusion, a Direct Challenge, the standing events — each with its window, its proof of work and its convergence |
| `publisher` | one queue ordered by deadline, the token governor with its reserve, stamping without backdating, the outcomes and what each convergence does with a missing acknowledgment, the lease on the host |
| `relay` | the relay's NIP-11 document, read as a self-timed client must: the `created_at` window, the covered kinds, the proof-of-work minimum |
| `publish` | the primitives: the relay's estimated clock, NIP-13 mining, `publish_self_timed` for a caller without the Publisher |
| `tags` | readers of the suite's tag conventions |
| `testing` | an in-process NIP-01 relay with the strict window, a clock skew and the proof of work switchable (feature `testing`) |

**The oracle.** Everything the rules decide — a move's legality, the canonical
chain, the clocks, a verdict — is asked of the session's module, never
recomputed here (ADR-0034). The native rules crate serves one purpose: writing
and reading the canonical PMN of *SEI Rules Document — Sanki*, in positions
the module produced.

## Use

```toml
[dependencies]
sashite-sanki-client = "0.1"

[dev-dependencies]
sashite-sanki-client = { version = "0.1", features = ["testing"] }
```

```rust
use sashite_sanki_client::{chain, module, rules, session};

// The rule system: the event by id, the module by digest, both cached.
let loaded = rules::load(&client, &http, &cache_dir, rules_id).await?;
let mut oracle = loaded.runtime;

// A session: its terms, its events, its live view now.
let terms = session::terms(&game_session, &founding)?;
let events = session::Events::from_relay(relay_events.iter(), &terms, loaded.describe.max_step);
let view = chain::session_view(&mut oracle, &terms, t0, &events, now)?;

// `view.moves` is the chain in canonical PMN: an SEI `search` is
// `{"position": terms.position, "moves": view.moves, ...}`.
```

```rust
use sashite_sanki_client::drafts::Ply;
use sashite_sanki_client::publisher::{Outcome, Publisher, Settings};

// One writer: the signer moves into the Publisher, which takes the lease.
let settings = Settings::from_relay_info(relay_url, &relay_info, rate_per_minute, data_dir);
let publisher = Publisher::open(client, signer, settings).await?;

// A draft is a function of its stamp; the Publisher stamps, mines, signs,
// sends, and says what became of it.
match publisher.publish(Ply { session, opponent, step, content, draw: false, not_before, not_after }).await {
    Outcome::Accepted(event) => { /* on the relay */ }
    Outcome::Withheld(why) => { /* expired, or moot */ }
    other => { /* rejected, unknown, failed, closed */ }
}
```

## What it does not do

It sends no challenge, founds no session, plays no move and claims no verdict
on its own; it holds no key beyond the signing a caller asks for — a signer
the caller moves into the `Publisher`; it has no configuration.

## Licence

Apache-2.0. Part of the [Sashité](https://sashite.com/) project.
