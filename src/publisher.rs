// SPDX-License-Identifier: Apache-2.0
//! The Publisher (ADR-0045 §6): **one queue, one writer**.
//!
//! - Every draft waits in a single queue ordered by `not_after`, earliest
//!   first; drafts without a deadline come after. A token governor admits at
//!   most `rate_per_minute` events in any **62 s** window (60 s plus the
//!   relay's whole seconds and jitter). Standing events and outgoing
//!   challenges may not take the last `⌈rate_per_minute / 10⌉` tokens: the
//!   **reserve**, for Game Sessions, Conclusions, retries and time trouble.
//!   Every event sent counts — a resend, a re-stamped draft, a retry — since
//!   the relay counts them all.
//! - **Stamping, without backdating.** The stamp is `max(floor(now) + 1,
//!   not_before)`, `now` being the relay's clock as estimated (the host's,
//!   NTP-synced, corrected by the skew the relay's rejections taught). A
//!   stamp above `not_after` is `Expired`, and nothing is published.
//! - **At open,** the publisher measures its publication latency — the
//!   relay's round trip (a `REQ` answered by `EOSE`) plus the mining
//!   benchmark's worst of twenty — and refuses to open when that reaches
//!   `past_tolerance + 1 s`, since its stamps would arrive stale; it also
//!   refuses a difficulty the host cannot mine within a second. Mining runs
//!   off the async executor.
//! - **Outcomes.** `OK true` is `Accepted`, a `duplicate:` message included.
//!   A rejection is classified by the reference relay's reason classes
//!   (`invalid:`, `pow:`, `rate-limited:`, `blocked:`); a timing rejection
//!   (`Stale`, `Future`) corrects the skew estimate and re-stamps once
//!   within the window; `Pow` re-reads NIP-11 and re-mines once;
//!   `RateLimited` — impossible with one writer by §4 — is logged at
//!   `error` and retried once; `Blocked` and `Invalid` are final. No
//!   acknowledgment is `Unknown`, and the draft's convergence decides: a Ply
//!   is sent again unchanged while its stamp is within the relay's past
//!   tolerance, then re-stamped; an `EarliestWins` draft is resolved by id
//!   and re-signed only if the relay answered that it is absent — a relay
//!   that does not answer proves nothing; a `NotIdempotent` draft is handed
//!   back as `Unknown` for the caller to resolve before sending another to
//!   the same target.
//! - **One writer on the host.** [`Publisher::open`] takes a [`Lease`]: a
//!   process-wide registry and an advisory lock on `<data_dir>/<pubkey>.lock`;
//!   a second instance fails with [`OpenError::KeyInUse`].
//!
//! Every event the publisher signs carries the NIP-89 `client` tag
//! [`CLIENT_TAG`], by which another instance of the library is told from a
//! person acting with the same key (ADR-0045 §2).

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use nostr_sdk::prelude::*;
use tokio::sync::{oneshot, Notify};

use crate::drafts::{Convergence, Publishable, Withheld};
use crate::publish::{is_future_reason, RelayClock};
use crate::relay::RelayInfo;

/// The NIP-89 `client` tag every event of this library carries.
pub const CLIENT_TAG: &str = "sanki-bot";

/// The governor's window: 60 s, plus the relay's whole seconds and jitter.
pub const RATE_WINDOW: Duration = Duration::from_secs(62);

/// How long the relay gets to acknowledge an event: short, so that a Ply
/// left unacknowledged can be sent again unchanged while its stamp is still
/// within the relay's past tolerance (a second on the reference relay); the
/// reference relay acknowledges in milliseconds, and a slow acknowledgment
/// costs a duplicate the relay accepts as such.
const OK_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a `resolve` query may take.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest mining time the open accepts, at the worst of twenty.
const MINING_BOUND: Duration = Duration::from_secs(1);

/// Sends a draft may take after a rejection: the first, and one retry
/// (a re-stamp, a re-mine, a pause) — ADR-0045 §6.
const MAX_REJECTED_SENDS: u32 = 2;

/// Sends a draft may take while unacknowledged (resends and re-stamps).
const MAX_UNKNOWN_SENDS: u32 = 4;

/// The pause before a retry after `RateLimited`.
const RATE_LIMITED_PAUSE: Duration = Duration::from_secs(2);

/// What the publisher needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The relay, the only one; the hint of every reference.
    pub relay: RelayUrl,
    /// The relay's per-key limit, events per minute.
    pub rate_per_minute: u32,
    /// Where the lease lives.
    pub data_dir: PathBuf,
    /// The relay's NIP-13 minimum on the kinds that prescribe a nonce.
    pub pow: u8,
    /// The relay's past tolerance on `created_at`, in seconds.
    pub past_tolerance: u64,
    /// The relay's future tolerance, in seconds.
    pub future_tolerance: u64,
}

impl Settings {
    /// Settings from the relay's document.
    #[must_use]
    pub fn from_relay_info(
        relay: RelayUrl,
        info: &RelayInfo,
        rate_per_minute: u32,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            relay,
            rate_per_minute,
            data_dir,
            pow: info.min_pow_difficulty(),
            past_tolerance: info.past_tolerance().unwrap_or(1),
            future_tolerance: info.future_tolerance(),
        }
    }

    /// The reserve: `⌈rate_per_minute / 10⌉` tokens.
    #[must_use]
    pub const fn reserve(&self) -> u32 {
        self.rate_per_minute.div_ceil(10)
    }
}

/// Why a rejection was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The stamp was in the relay's past.
    Stale,
    /// The stamp was in the relay's future.
    Future,
    /// The proof of work was below the minimum.
    Pow,
    /// Over the per-key rate.
    RateLimited,
    /// Blocked by the relay's policy.
    Blocked,
    /// Refused as invalid, for the reason given.
    Invalid(String),
}

impl Rejection {
    /// Classifies the relay's reason for an event stamped `stamp`, the
    /// relay's clock being `now` by the host's estimate: the class comes
    /// from the reference relay's reason prefix, the timing direction from
    /// the stamp against the window — and, when the stamp was inside it by
    /// the host's clock (a skew), from the reason's wording.
    #[must_use]
    pub fn classify(reason: &str, stamp: u64, now: u64, settings: &Settings) -> Self {
        let lower = reason.trim().to_lowercase();
        if lower.starts_with("pow:") {
            return Self::Pow;
        }
        if lower.starts_with("rate-limited:") {
            return Self::RateLimited;
        }
        if lower.starts_with("blocked:")
            || lower.starts_with("restricted:")
            || lower.starts_with("auth-required:")
        {
            return Self::Blocked;
        }
        if lower.starts_with("invalid:") {
            if stamp.saturating_add(settings.past_tolerance) < now {
                return Self::Stale;
            }
            if stamp > now.saturating_add(settings.future_tolerance) {
                return Self::Future;
            }
            if lower.contains("created_at") || lower.contains("timestamp") {
                return if is_future_reason(reason) {
                    Self::Future
                } else {
                    Self::Stale
                };
            }
        }
        Self::Invalid(reason.to_owned())
    }

    /// Whether the relay answered with a verdict: a reason of one of its
    /// classes. An `error:` (a relay-side failure) and anything else are
    /// not verdicts: the send is `Unknown`, and the convergence decides.
    fn is_relay_verdict(reason: &str) -> bool {
        let lower = reason.trim().to_lowercase();
        [
            "invalid:",
            "pow:",
            "rate-limited:",
            "blocked:",
            "duplicate:",
            "restricted:",
            "auth-required:",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stale => f.write_str("stale created_at"),
            Self::Future => f.write_str("future created_at"),
            Self::Pow => f.write_str("proof of work below the minimum"),
            Self::RateLimited => f.write_str("rate limited"),
            Self::Blocked => f.write_str("blocked"),
            Self::Invalid(reason) => write!(f, "invalid: {reason}"),
        }
    }
}

/// What became of a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The relay accepted the event.
    Accepted(Event),
    /// The relay rejected it, finally.
    Rejected(Rejection),
    /// The event was sent and never acknowledged; the caller resolves it.
    Unknown(Event),
    /// The draft yielded no event.
    Withheld(Withheld),
    /// The event could not be built, mined or signed — a defect of the
    /// host, never of the relay; logged at `error`.
    Failed(String),
    /// The publisher is closed.
    Closed,
}

/// What a query for an id found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The relay holds it.
    Found(Event),
    /// The relay answered, and does not hold it.
    ConfirmedAbsent,
    /// The relay did not answer.
    Unknown,
}

/// Why the publisher could not open.
#[derive(Debug)]
#[non_exhaustive]
pub enum OpenError {
    /// Another instance holds the key on this host.
    KeyInUse(PathBuf),
    /// The lock file cannot be used.
    Lock(PathBuf, std::io::Error),
    /// The relay did not answer the round-trip probe.
    Unreachable,
    /// The latency bound: the round trip plus the mining benchmark, and
    /// the bound they reached.
    Latency {
        /// The relay's round trip.
        round_trip: Duration,
        /// The worst of twenty minings.
        mining: Duration,
        /// `past_tolerance + 1 s`.
        bound: Duration,
    },
    /// The host cannot mine the difficulty within a second.
    Mining {
        /// The difficulty.
        difficulty: u8,
        /// The worst of twenty.
        worst: Duration,
    },
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyInUse(path) => write!(
                f,
                "another instance holds this key on this host ({})",
                path.display()
            ),
            Self::Lock(path, err) => write!(f, "{}: {err}", path.display()),
            Self::Unreachable => f.write_str("the relay does not answer"),
            Self::Latency {
                round_trip,
                mining,
                bound,
            } => write!(
                f,
                "publication latency {:?} + {:?} reaches {:?}: stamps would arrive stale",
                round_trip, mining, bound
            ),
            Self::Mining { difficulty, worst } => write!(
                f,
                "difficulty {difficulty} takes up to {worst:?} to mine, above a second"
            ),
        }
    }
}

impl std::error::Error for OpenError {}

// ---- the lease ----

/// The keys leased in this process.
fn registry() -> &'static Mutex<BTreeSet<PublicKey>> {
    static REGISTRY: OnceLock<Mutex<BTreeSet<PublicKey>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// The lease of a key on this host: held until dropped.
#[derive(Debug)]
pub struct Lease {
    pubkey: PublicKey,
    path: PathBuf,
    _lock: nix::fcntl::Flock<File>,
}

impl Lease {
    /// Takes the lease of `pubkey` under `data_dir`.
    ///
    /// # Errors
    ///
    /// [`OpenError::KeyInUse`] when this process or another holds it;
    /// [`OpenError::Lock`] when the lock file cannot be opened.
    pub fn take(data_dir: &Path, pubkey: PublicKey) -> Result<Self, OpenError> {
        let path = data_dir.join(format!("{}.lock", pubkey.to_hex()));
        {
            let mut held = registry()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !held.insert(pubkey) {
                return Err(OpenError::KeyInUse(path));
            }
        }
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) => {
                Self::release(pubkey);
                return Err(OpenError::Lock(path, e));
            }
        };
        match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
            Ok(lock) => Ok(Self {
                pubkey,
                path,
                _lock: lock,
            }),
            Err((_, _)) => {
                Self::release(pubkey);
                Err(OpenError::KeyInUse(path))
            }
        }
    }

    /// The lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn release(pubkey: PublicKey) {
        let mut held = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.remove(&pubkey);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        Self::release(self.pubkey);
    }
}

// ---- the governor ----

/// The token governor: at most `rate` sends in any window.
#[derive(Debug)]
struct Governor {
    rate: u32,
    sends: VecDeque<Instant>,
}

impl Governor {
    fn prune(&mut self, now: Instant) {
        while self
            .sends
            .front()
            .is_some_and(|sent| now.saturating_duration_since(*sent) >= RATE_WINDOW)
        {
            self.sends.pop_front();
        }
    }

    /// Tokens available now.
    fn available(&mut self, now: Instant) -> u32 {
        self.prune(now);
        self.rate
            .saturating_sub(u32::try_from(self.sends.len()).unwrap_or(u32::MAX))
    }

    /// Takes a token when at least `keep + 1` are available.
    fn take(&mut self, now: Instant, keep: u32) -> bool {
        if self.available(now) > keep {
            self.sends.push_back(now);
            true
        } else {
            false
        }
    }

    /// When the next token frees.
    fn next_free(&self) -> Option<Instant> {
        self.sends
            .front()
            .and_then(|sent| sent.checked_add(RATE_WINDOW))
    }
}

// ---- the queue ----

struct Job {
    seq: u64,
    draft: Box<dyn Publishable>,
    reply: oneshot::Sender<Outcome>,
    attempts: u32,
    /// Earliest instant the job may be dispatched (a retry's pause).
    not_before_instant: Option<Instant>,
}

struct Inner<S> {
    /// The lease, held as long as any job may still send.
    _lease: Lease,
    client: Client,
    signer: S,
    settings: Settings,
    relay_hint: String,
    relay_clock: RelayClock,
    pow: AtomicU64,
    governor: Mutex<Governor>,
    queue: Mutex<Vec<Job>>,
    wake: Notify,
    seq: AtomicU64,
    closed: AtomicBool,
    http: reqwest::Client,
    /// The last stamp accepted per replaceable coordinate (kind, `d`), so
    /// that the stamp is monotone there and a second write within the same
    /// second is not lost to the lower id.
    last_replaceable: Mutex<std::collections::HashMap<(u16, String), u64>>,
}

/// The publisher: one queue, one writer.
pub struct Publisher<S> {
    inner: Arc<Inner<S>>,
}

impl<S> fmt::Debug for Publisher<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Publisher")
            .field("relay", &self.inner.settings.relay)
            .finish_non_exhaustive()
    }
}

/// The seconds of the relay's clock as the publisher estimates it.
fn floor_now(clock: &RelayClock) -> u64 {
    clock.now_secs()
}

impl<S> Publisher<S>
where
    S: GetPublicKey + SignEvent + Send + Sync + 'static,
{
    /// Opens the publisher: the lease, the relay's round trip, the mining
    /// benchmark, the latency bound; then the dispatcher runs until the
    /// publisher is dropped. `client` is connected to the relay already.
    ///
    /// # Errors
    ///
    /// See [`OpenError`].
    pub async fn open(client: Client, signer: S, settings: Settings) -> Result<Self, OpenError> {
        let pubkey = signer
            .get_public_key()
            .map_err(|_| OpenError::Unreachable)?;
        let lease = Lease::take(&settings.data_dir, pubkey)?;

        // The round trip: a REQ answered by EOSE, on the relay itself (the
        // pool's fetch ends silently on a timeout; the relay's propagates
        // its errors), the worst of three.
        let mut round_trip = Duration::ZERO;
        for _ in 0..3 {
            let started = Instant::now();
            if fetch_by_id(&client, &settings.relay, EventId::from_byte_array([0; 32]))
                .await
                .is_none()
            {
                return Err(OpenError::Unreachable);
            }
            round_trip = round_trip.max(started.elapsed());
        }

        // The mining benchmark, off the executor.
        let difficulty = settings.pow;
        let mining = tokio::task::spawn_blocking(move || benchmark(difficulty, pubkey))
            .await
            .unwrap_or(MINING_BOUND);
        if mining > MINING_BOUND {
            return Err(OpenError::Mining {
                difficulty,
                worst: mining,
            });
        }
        // A stamp `floor(now) + 1` arrives at the relay `mining + transit`
        // later; it is stale once that exceeds the past tolerance, whatever
        // the fraction of the second. The whole round trip stands for the
        // transit, conservatively.
        let bound = Duration::from_secs(settings.past_tolerance);
        if round_trip.saturating_add(mining) > bound {
            return Err(OpenError::Latency {
                round_trip,
                mining,
                bound,
            });
        }

        let inner = Arc::new(Inner {
            _lease: lease,
            client,
            signer,
            relay_hint: settings.relay.to_string(),
            pow: AtomicU64::new(u64::from(settings.pow)),
            governor: Mutex::new(Governor {
                rate: settings.rate_per_minute,
                sends: VecDeque::new(),
            }),
            settings,
            relay_clock: RelayClock::new(),
            queue: Mutex::new(Vec::new()),
            wake: Notify::new(),
            seq: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            http: reqwest::Client::new(),
            last_replaceable: Mutex::new(std::collections::HashMap::new()),
        });
        tokio::spawn(dispatch(Arc::clone(&inner)));
        Ok(Self { inner })
    }

    /// The bot's public key.
    #[must_use]
    pub fn public_key(&self) -> Option<PublicKey> {
        self.inner.signer.get_public_key().ok()
    }

    /// The relay's clock now, in seconds, as estimated.
    #[must_use]
    pub fn now(&self) -> u64 {
        floor_now(&self.inner.relay_clock)
    }

    /// The stamp an event published now would carry: `floor(now) + 1`.
    #[must_use]
    pub fn stamp(&self) -> u64 {
        self.now().saturating_add(1)
    }

    /// The settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// Queues a draft and waits for its outcome.
    pub async fn publish(&self, draft: impl Publishable + 'static) -> Outcome {
        self.publish_boxed(Box::new(draft)).await
    }

    /// Queues a boxed draft and waits for its outcome.
    pub async fn publish_boxed(&self, draft: Box<dyn Publishable>) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        self.inner.enqueue(Job {
            seq: self.inner.seq.fetch_add(1, Ordering::Relaxed),
            draft,
            reply,
            attempts: 0,
            not_before_instant: None,
        });
        outcome.await.unwrap_or(Outcome::Closed)
    }

    /// Asks the relay for `id`.
    pub async fn resolve(&self, id: EventId) -> Resolution {
        self.inner.resolve(id).await
    }

    /// Stops the dispatcher: queued drafts, and every draft queued after,
    /// are answered `Closed`; jobs in flight finish, and the lease is
    /// released once they have.
    pub fn close(&self) {
        self.inner.close();
    }
}

impl<S> Drop for Publisher<S> {
    fn drop(&mut self) {
        {
            let _queue = self
                .inner
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner.closed.store(true, Ordering::Relaxed);
        }
        self.inner.wake.notify_one();
    }
}

/// Fetches `id` from the relay itself, under a timeout of this crate's:
/// `None` when the relay did not answer (a timeout, a disconnection), so
/// that silence is never read as absence.
async fn fetch_by_id(client: &Client, relay: &RelayUrl, id: EventId) -> Option<Vec<Event>> {
    let relay = client.relay(relay).await.ok()??;
    let fetched = tokio::time::timeout(
        RESOLVE_TIMEOUT,
        relay.fetch_events(vec![Filter::new().id(id).limit(1)]),
    )
    .await;
    match fetched {
        Ok(Ok(events)) => Some(events.into_iter().collect()),
        _ => None,
    }
}

/// Mines twenty throwaway events at `difficulty`; the worst time.
fn benchmark(difficulty: u8, pubkey: PublicKey) -> Duration {
    let Some(target) = std::num::NonZeroU8::new(difficulty) else {
        return Duration::ZERO;
    };
    let mut worst = Duration::ZERO;
    for i in 0..20u32 {
        let started = Instant::now();
        let unsigned = EventBuilder::new(Kind::Custom(3423), format!("benchmark {i}"))
            .finalize_unsigned(pubkey);
        let _ = unsigned.mine(&SingleThreadPow, target);
        worst = worst.max(started.elapsed());
        if worst > MINING_BOUND {
            break;
        }
    }
    worst
}

impl<S> Inner<S>
where
    S: GetPublicKey + SignEvent + Send + Sync + 'static,
{
    /// Queues a job — or answers it `Closed` when the publisher is closed;
    /// both under the queue's lock, so that no job is queued after the
    /// dispatcher's last drain.
    fn enqueue(&self, job: Job) {
        {
            let mut queue = self
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if self.closed.load(Ordering::Relaxed) {
                let _ = job.reply.send(Outcome::Closed);
                return;
            }
            queue.push(job);
        }
        self.wake.notify_one();
    }

    fn close(&self) {
        {
            let _queue = self
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.closed.store(true, Ordering::Relaxed);
        }
        self.wake.notify_one();
    }

    fn difficulty(&self) -> u8 {
        u8::try_from(self.pow.load(Ordering::Relaxed)).unwrap_or(u8::MAX)
    }

    async fn resolve(&self, id: EventId) -> Resolution {
        match fetch_by_id(&self.client, &self.settings.relay, id).await {
            Some(events) => match events.into_iter().find(|event| event.id == id) {
                Some(event) => Resolution::Found(event),
                None => Resolution::ConfirmedAbsent,
            },
            None => Resolution::Unknown,
        }
    }

    /// The stamp of a draft now: `max(floor(now) + 1, not_before)`, and,
    /// for a replaceable draft, above the last stamp of its coordinate.
    fn stamp_for(&self, draft: &dyn Publishable) -> u64 {
        let now = floor_now(&self.relay_clock);
        let mut stamp = now
            .saturating_add(1)
            .max(draft.window().not_before.unwrap_or(0));
        if draft.convergence() == Convergence::Replaceable {
            let last = self
                .last_replaceable
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(previous) = last.get(&coordinate(draft)) {
                stamp = stamp.max(previous.saturating_add(1));
            }
        }
        stamp
    }

    fn record_replaceable(&self, draft: &dyn Publishable, stamp: u64) {
        if draft.convergence() == Convergence::Replaceable {
            let mut last = self
                .last_replaceable
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = last.entry(coordinate(draft)).or_insert(0);
            *entry = (*entry).max(stamp);
        }
    }

    /// Builds, mines and signs the draft at `stamp`.
    async fn sign(&self, draft: &dyn Publishable, stamp: u64) -> Result<Event, SignFailure> {
        let parts = draft
            .at(stamp, &self.relay_hint)
            .map_err(SignFailure::Withheld)?;
        let mut tags = parts.tags;
        tags.push(Tag::custom("client", [CLIENT_TAG]));
        let mined = draft.mined();
        let difficulty = self.difficulty();
        if mined && difficulty == 0 {
            tags.push(Tag::custom("nonce", ["0", "0"]));
        }
        let Ok(pubkey) = self.signer.get_public_key() else {
            return Err(SignFailure::Failed(
                "the signer has no public key".to_owned(),
            ));
        };
        let builder = EventBuilder::new(draft.kind(), parts.content)
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(stamp));
        let unsigned = builder.finalize_unsigned(pubkey);
        let unsigned = match (mined, std::num::NonZeroU8::new(difficulty)) {
            (true, Some(target)) => tokio::task::spawn_blocking(move || {
                unsigned
                    .mine(&SingleThreadPow, target)
                    .map_err(|e| SignFailure::Failed(format!("mining: {e}")))
            })
            .await
            .unwrap_or_else(|e| Err(SignFailure::Failed(format!("mining task: {e}"))))?,
            _ => unsigned,
        };
        self.signer
            .sign_event(unsigned)
            .map_err(|e| SignFailure::Failed(format!("signing: {e}")))
    }

    /// Sends a signed event; the relay's answer.
    async fn send(&self, event: &Event) -> Sent {
        let output = self.client.send_event(event).ok_timeout(OK_TIMEOUT).await;
        match output {
            Ok(output) if !output.success.is_empty() => Sent::Accepted,
            Ok(output) => {
                let reason = output.failed.values().next().cloned().unwrap_or_default();
                if reason.trim().to_lowercase().starts_with("duplicate:") {
                    Sent::Accepted
                } else if Rejection::is_relay_verdict(&reason) {
                    Sent::Rejected(reason)
                } else {
                    Sent::Unknown
                }
            }
            Err(_) => Sent::Unknown,
        }
    }

    /// One attempt at a job; the job comes back for a retry, or its outcome
    /// is delivered.
    async fn attempt(self: Arc<Self>, mut job: Job) {
        let settings = &self.settings;
        let window = job.draft.window();
        let stamp = self.stamp_for(job.draft.as_ref());
        if window.not_after.is_some_and(|after| stamp > after) {
            let _ = job.reply.send(Outcome::Withheld(Withheld::Expired));
            return;
        }
        let name = job.draft.name();
        let event = match self.sign(job.draft.as_ref(), stamp).await {
            Ok(event) => event,
            Err(SignFailure::Withheld(withheld)) => {
                let _ = job.reply.send(Outcome::Withheld(withheld));
                return;
            }
            Err(SignFailure::Failed(reason)) => {
                tracing::error!(draft = name, %reason, "the event could not be built");
                let _ = job.reply.send(Outcome::Failed(reason));
                return;
            }
        };
        job.attempts = job.attempts.saturating_add(1);
        match self.send(&event).await {
            Sent::Accepted => {
                self.record_replaceable(job.draft.as_ref(), stamp);
                let _ = job.reply.send(Outcome::Accepted(event));
            }
            Sent::Rejected(reason) => {
                let rejection =
                    Rejection::classify(&reason, stamp, floor_now(&self.relay_clock), settings);
                let retry = match &rejection {
                    Rejection::Stale => {
                        self.relay_clock.bump();
                        tracing::debug!(draft = name, %reason, "stale stamp; re-stamping");
                        true
                    }
                    Rejection::Future => {
                        self.relay_clock.lower();
                        tracing::debug!(draft = name, %reason, "future stamp; re-stamping");
                        true
                    }
                    Rejection::Pow => {
                        self.reread_pow().await;
                        tracing::warn!(draft = name, %reason, "proof of work refused; re-mining");
                        true
                    }
                    Rejection::RateLimited => {
                        tracing::error!(draft = name, %reason, "rate limited: another writer, or a wrong rate_per_minute");
                        job.not_before_instant = Instant::now().checked_add(RATE_LIMITED_PAUSE);
                        true
                    }
                    Rejection::Blocked | Rejection::Invalid(_) => false,
                };
                if retry && job.attempts < MAX_REJECTED_SENDS {
                    self.enqueue(job);
                } else {
                    if retry {
                        tracing::error!(draft = name, %reason, "kept rejected; NTP or the relay's policy suspected");
                    }
                    let _ = job.reply.send(Outcome::Rejected(rejection));
                }
            }
            Sent::Unknown => match job.draft.convergence() {
                Convergence::Collapses => {
                    if job.attempts >= MAX_UNKNOWN_SENDS {
                        let _ = job.reply.send(Outcome::Unknown(event));
                        return;
                    }
                    // The same signed event again while its stamp is within
                    // the relay's past tolerance — with a token taken at
                    // once, never waited for, so that the freshness judged
                    // here still holds at the send; past that, re-stamped
                    // through the queue.
                    let still_fresh = floor_now(&self.relay_clock)
                        <= stamp.saturating_add(settings.past_tolerance);
                    if still_fresh && self.take_token_now(true) {
                        job.attempts = job.attempts.saturating_add(1);
                        match self.send(&event).await {
                            Sent::Accepted => {
                                let _ = job.reply.send(Outcome::Accepted(event));
                            }
                            Sent::Rejected(reason) => {
                                // The resend met a verdict: the relay saw the
                                // event, and says why not. A timing verdict
                                // is re-stamped through the queue; any other
                                // is final.
                                let rejection = Rejection::classify(
                                    &reason,
                                    stamp,
                                    floor_now(&self.relay_clock),
                                    settings,
                                );
                                match rejection {
                                    Rejection::Stale | Rejection::Future => self.enqueue(job),
                                    other => {
                                        let _ = job.reply.send(Outcome::Rejected(other));
                                    }
                                }
                            }
                            Sent::Unknown => self.enqueue(job),
                        }
                    } else {
                        self.enqueue(job);
                    }
                }
                Convergence::EarliestWins => match self.resolve(event.id).await {
                    Resolution::Found(found) => {
                        let _ = job.reply.send(Outcome::Accepted(found));
                    }
                    Resolution::ConfirmedAbsent if job.attempts < MAX_UNKNOWN_SENDS => {
                        self.enqueue(job);
                    }
                    _ => {
                        let _ = job.reply.send(Outcome::Unknown(event));
                    }
                },
                Convergence::Replaceable if job.attempts < MAX_UNKNOWN_SENDS => self.enqueue(job),
                Convergence::Replaceable | Convergence::NotIdempotent => {
                    let _ = job.reply.send(Outcome::Unknown(event));
                }
            },
        }
    }

    /// Re-reads the relay's NIP-11 document for its difficulty.
    async fn reread_pow(&self) {
        if let Ok(info) = RelayInfo::fetch(&self.http, &self.settings.relay.to_string()).await {
            let min = u64::from(info.min_pow_difficulty());
            let _ = self
                .pow
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.max(min))
                });
        }
    }

    /// Takes a token now, outside the queue's order (a resend), or not.
    fn take_token_now(&self, may_take_reserve: bool) -> bool {
        let keep = if may_take_reserve {
            0
        } else {
            self.settings.reserve()
        };
        let mut governor = self
            .governor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        governor.take(Instant::now(), keep)
    }
}

/// Why a draft could not be signed.
enum SignFailure {
    Withheld(Withheld),
    Failed(String),
}

/// The replaceable coordinate of a draft: its kind and its `d` tag.
fn coordinate(draft: &dyn Publishable) -> (u16, String) {
    let d = draft
        .at(0, "")
        .ok()
        .and_then(|parts| {
            parts
                .tags
                .iter()
                .map(Tag::as_slice)
                .find(|s| s.first().map(String::as_str) == Some("d"))
                .and_then(|s| s.get(1).cloned())
        })
        .unwrap_or_default();
    (draft.kind().as_u16(), d)
}

/// What the relay answered.
enum Sent {
    Accepted,
    Rejected(String),
    Unknown,
}

/// What the dispatcher does next.
enum Next {
    Run(Job),
    Wait(Instant),
    Idle,
}

/// The dispatcher: picks the earliest-deadline job that may run now.
async fn dispatch<S>(inner: Arc<Inner<S>>)
where
    S: GetPublicKey + SignEvent + Send + Sync + 'static,
{
    loop {
        if inner.closed.load(Ordering::Relaxed) {
            let jobs: Vec<Job> = std::mem::take(
                &mut *inner
                    .queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            for job in jobs {
                let _ = job.reply.send(Outcome::Closed);
            }
            return;
        }
        let next = choose(&inner);
        match next {
            Next::Run(job) => {
                tokio::spawn(Arc::clone(&inner).attempt(job));
            }
            Next::Wait(until) => {
                tokio::select! {
                    () = inner.wake.notified() => {}
                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(until)) => {}
                }
            }
            Next::Idle => inner.wake.notified().await,
        }
    }
}

/// The choice: among the jobs, ordered by `not_after` then arrival, the
/// first whose `not_before` has come and for which a token is available.
fn choose<S>(inner: &Inner<S>) -> Next {
    let now_secs = floor_now(&inner.relay_clock);
    let now = Instant::now();
    let mut queue = inner
        .queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut governor = inner
        .governor
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let reserve = inner.settings.reserve();

    // Expired drafts are answered at once.
    let mut index = 0;
    while index < queue.len() {
        // The stamp would be `now_secs + 1`: past `not_after`, nothing to
        // send.
        let expired = queue.get(index).is_some_and(|job| {
            job.draft
                .window()
                .not_after
                .is_some_and(|after| now_secs.saturating_add(1) > after)
        });
        if expired {
            let job = queue.remove(index);
            let _ = job.reply.send(Outcome::Withheld(Withheld::Expired));
        } else {
            index = index.saturating_add(1);
        }
    }

    let mut order: Vec<usize> = (0..queue.len()).collect();
    order.sort_by_key(|i| {
        queue
            .get(*i)
            .map(|job| (job.draft.window().not_after.unwrap_or(u64::MAX), job.seq))
            .unwrap_or((u64::MAX, u64::MAX))
    });
    let mut earliest_wait: Option<Instant> = None;
    for i in order {
        let Some(job) = queue.get(i) else { continue };
        let window = job.draft.window();
        let ready_at = window
            .not_before
            .filter(|before| *before > now_secs.saturating_add(1))
            .map(|before| {
                now.checked_add(Duration::from_secs(
                    before.saturating_sub(now_secs).saturating_sub(1),
                ))
                .unwrap_or(now)
            });
        let ready_at = match (ready_at, job.not_before_instant) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
        .filter(|at| *at > now);
        if let Some(at) = ready_at {
            earliest_wait = Some(earliest_wait.map_or(at, |w| w.min(at)));
            continue;
        }
        let keep = if job.draft.may_take_reserve() {
            0
        } else {
            reserve
        };
        if governor.take(now, keep) {
            let job = queue.remove(i);
            return Next::Run(job);
        }
        if let Some(free) = governor.next_free() {
            earliest_wait = Some(earliest_wait.map_or(free, |w| w.min(free)));
        }
    }
    match earliest_wait {
        Some(until) => Next::Wait(until),
        None => Next::Idle,
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

    fn settings() -> Settings {
        Settings {
            relay: RelayUrl::parse("wss://relay.sanki.app").unwrap(),
            rate_per_minute: 30,
            data_dir: std::env::temp_dir(),
            pow: 10,
            past_tolerance: 1,
            future_tolerance: 5,
        }
    }

    #[test]
    fn classifies_by_class_then_by_computation() {
        let s = settings();
        assert_eq!(
            Rejection::classify(
                "pow: difficulty 3 is below the required minimum 10",
                100,
                100,
                &s
            ),
            Rejection::Pow
        );
        assert_eq!(
            Rejection::classify(
                "rate-limited: over 30 events per 60s from this signer",
                100,
                100,
                &s
            ),
            Rejection::RateLimited
        );
        assert_eq!(
            Rejection::classify("blocked: kind 1 is not served here", 100, 100, &s),
            Rejection::Blocked
        );
        // Outside the window by the host's clock.
        assert_eq!(
            Rejection::classify(
                "invalid: created_at 95 is in the past (relay clock 100, tolerance 1s)",
                95,
                100,
                &s
            ),
            Rejection::Stale
        );
        assert_eq!(
            Rejection::classify(
                "invalid: timestamp 110 is too far in the future (relay clock 100, tolerance 5s)",
                110,
                100,
                &s
            ),
            Rejection::Future
        );
        // Inside it: a skew; the wording says which way.
        assert_eq!(
            Rejection::classify(
                "invalid: created_at 101 is in the past (relay clock 104, tolerance 1s)",
                101,
                100,
                &s
            ),
            Rejection::Stale
        );
        assert_eq!(
            Rejection::classify(
                "invalid: timestamp 101 is too far in the future (relay clock 90, tolerance 5s)",
                101,
                100,
                &s
            ),
            Rejection::Future
        );
        // Another invalid.
        assert!(matches!(
            Rejection::classify("invalid: slot ceiling reached", 101, 100, &s),
            Rejection::Invalid(_)
        ));
        assert!(Rejection::is_relay_verdict(
            "duplicate: already have this event"
        ));
        assert!(!Rejection::is_relay_verdict("timeout"));
    }

    #[test]
    fn the_governor_keeps_the_reserve() {
        let mut g = Governor {
            rate: 10,
            sends: VecDeque::new(),
        };
        let now = Instant::now();
        for _ in 0..9 {
            assert!(g.take(now, 0));
        }
        // One token left: the reserve (⌈10/10⌉ = 1) keeps it from a
        // standing event, not from a Ply.
        assert!(!g.take(now, 1));
        assert!(g.take(now, 0));
        assert!(!g.take(now, 0));
        assert_eq!(g.next_free(), Some(now + RATE_WINDOW));
        assert_eq!(g.available(now + RATE_WINDOW), 10);
    }

    #[test]
    fn the_reserve_is_a_tenth_rounded_up() {
        assert_eq!(settings().reserve(), 3);
        let mut s = settings();
        s.rate_per_minute = 1;
        assert_eq!(s.reserve(), 1);
    }
}
