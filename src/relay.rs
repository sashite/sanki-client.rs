// SPDX-License-Identifier: Apache-2.0
//! The relay's information document (NIP-11), read the way a self-timed
//! client must read it (ADR-0045 §1 *Relay information*; *Self-Timed Timing
//! Relay* §NIP-11 advertisement):
//!
//! - `limitation.created_at_lower_limit` — seconds into the PAST the relay
//!   still accepts a `created_at`. The anti-backdating rule a self-timed
//!   session's timing rests on: a consumer relies on the relay's timing only
//!   when this bound is advertised and tight;
//! - `limitation.created_at_upper_limit` — seconds into the FUTURE. Hygiene;
//! - `limitation.created_at_window_kinds` — the kinds the window COVERS.
//!   Absent: every kind. Present: only those, and the session kinds must be
//!   among them;
//! - `limitation.min_pow_difficulty` — the NIP-13 minimum on the kinds that
//!   prescribe a `nonce` tag. Absent: `0`.
//!
//! The document is fetched over HTTPS from the relay's own host, with the
//! `Accept: application/nostr+json` header NIP-11 requires.

use serde::Deserialize;
use std::fmt;
use std::time::Duration;

/// The kinds a self-timed session's timing rests on, which the relay's
/// window must cover: the Direct Challenge, the Game Session, the Ply and
/// the Conclusion (ADR-0045 §1).
pub const SESSION_KINDS: [u16; 4] = [3420, 3422, 3423, 3425];

/// The kinds of the pool (ADR-0047): the Open Challenge — never timed, but
/// written with the bot's key, so watched by an echo detector — and the
/// Pairing, whose canonical timing is its `created_at` and which the
/// relay's window must therefore cover for a bot that enters the pool.
pub const POOL_KINDS: [u16; 2] = [3418, 3419];

/// The widest past tolerance still countable as the strict anti-backdating
/// rule, in seconds: the reference relay enforces 1 s; a relay admitting
/// more than a few seconds is useless as a timing source.
pub const STRICT_PAST_TOLERANCE_MAX: u64 = 5;

/// The reference relay's default future tolerance, in seconds — assumed when
/// the document does not advertise one (the bound is hygiene, not trust).
pub const DEFAULT_FUTURE_TOLERANCE: u64 = 5;

/// How long the relay's host gets to serve its document.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// The members of a NIP-11 document this client reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RelayInfo {
    /// The relay's name, when advertised.
    #[serde(default)]
    pub name: Option<String>,
    /// The relay's software URL, when advertised.
    #[serde(default)]
    pub software: Option<String>,
    /// The `limitation` object.
    #[serde(default)]
    pub limitation: Limitation,
}

/// The `limitation` members this client reads. A member of the wrong JSON
/// type reads as absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Limitation {
    /// Seconds into the past the relay still accepts a `created_at`.
    #[serde(default, deserialize_with = "lenient")]
    pub created_at_lower_limit: Option<u64>,
    /// Seconds into the future the relay still accepts a `created_at`.
    #[serde(default, deserialize_with = "lenient")]
    pub created_at_upper_limit: Option<u64>,
    /// The kinds the window covers; absent means every kind.
    #[serde(default, deserialize_with = "lenient")]
    pub created_at_window_kinds: Option<Vec<u16>>,
    /// The NIP-13 minimum difficulty.
    #[serde(default, deserialize_with = "lenient")]
    pub min_pow_difficulty: Option<u8>,
}

/// Deserializes a member as `None` when it is absent or of the wrong type,
/// instead of failing the whole document.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).ok())
}

/// Why a relay's document does not let a self-timed client start (ADR-0045
/// §1: the start is refused).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelayInfoError {
    /// The document could not be fetched or is not JSON.
    Unavailable(String),
    /// No `created_at_lower_limit` is advertised: the relay does not
    /// verifiably enforce the anti-backdating rule.
    PastBoundMissing,
    /// The advertised past bound is above [`STRICT_PAST_TOLERANCE_MAX`].
    PastBoundLoose(u64),
    /// The window's covered kinds leave a session kind out.
    KindUncovered(u16),
}

impl fmt::Display for RelayInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "the relay's NIP-11 document: {reason}"),
            Self::PastBoundMissing => f.write_str(
                "the relay advertises no created_at_lower_limit: it is not a self-timed timing relay",
            ),
            Self::PastBoundLoose(secs) => write!(
                f,
                "the relay accepts a created_at {secs} s into the past, above {STRICT_PAST_TOLERANCE_MAX} s: it is not a strict timing relay"
            ),
            Self::KindUncovered(kind) => write!(
                f,
                "the relay's created_at window does not cover kind {kind}"
            ),
        }
    }
}

impl std::error::Error for RelayInfoError {}

impl RelayInfo {
    /// Reads a document from its JSON text.
    ///
    /// # Errors
    ///
    /// [`RelayInfoError::Unavailable`] when the text is not a JSON object.
    pub fn from_json(text: &str) -> Result<Self, RelayInfoError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| RelayInfoError::Unavailable(e.to_string()))?;
        if !value.is_object() {
            return Err(RelayInfoError::Unavailable(
                "the document is not a JSON object".to_owned(),
            ));
        }
        serde_json::from_value(value).map_err(|e| RelayInfoError::Unavailable(e.to_string()))
    }

    /// Fetches the document of the relay at `relay_url` (`wss://…` or
    /// `ws://…`, mapped to `https://…` or `http://…`).
    ///
    /// # Errors
    ///
    /// [`RelayInfoError::Unavailable`] with the transport's reason.
    pub async fn fetch(http: &reqwest::Client, relay_url: &str) -> Result<Self, RelayInfoError> {
        let url = info_url(relay_url);
        let response = http
            .get(&url)
            .header("Accept", "application/nostr+json")
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .map_err(|e| RelayInfoError::Unavailable(format!("{url}: {e}")))?;
        if !response.status().is_success() {
            return Err(RelayInfoError::Unavailable(format!(
                "{url}: HTTP {}",
                response.status()
            )));
        }
        let text = response
            .text()
            .await
            .map_err(|e| RelayInfoError::Unavailable(format!("{url}: {e}")))?;
        Self::from_json(&text)
    }

    /// The NIP-13 minimum the relay enforces; `0` when none is advertised.
    #[must_use]
    pub fn min_pow_difficulty(&self) -> u8 {
        self.limitation.min_pow_difficulty.unwrap_or(0)
    }

    /// The past tolerance the relay advertises, when it does.
    #[must_use]
    pub const fn past_tolerance(&self) -> Option<u64> {
        self.limitation.created_at_lower_limit
    }

    /// The future tolerance: advertised, else the reference default.
    #[must_use]
    pub fn future_tolerance(&self) -> u64 {
        self.limitation
            .created_at_upper_limit
            .unwrap_or(DEFAULT_FUTURE_TOLERANCE)
    }

    /// Whether the relay's window covers `kind`: an absent list covers every
    /// kind; a present one must name it.
    #[must_use]
    pub fn covers(&self, kind: u16) -> bool {
        self.limitation
            .created_at_window_kinds
            .as_ref()
            .is_none_or(|kinds| kinds.contains(&kind))
    }

    /// Whether the document lets a self-timed client start: the past bound
    /// is advertised and at most [`STRICT_PAST_TOLERANCE_MAX`], and the
    /// window covers every kind of [`SESSION_KINDS`].
    ///
    /// # Errors
    ///
    /// The first condition not met.
    pub fn check_self_timed(&self) -> Result<(), RelayInfoError> {
        match self.past_tolerance() {
            None => return Err(RelayInfoError::PastBoundMissing),
            Some(secs) if secs > STRICT_PAST_TOLERANCE_MAX => {
                return Err(RelayInfoError::PastBoundLoose(secs));
            }
            Some(_) => {}
        }
        if let Some(kind) = SESSION_KINDS.iter().find(|kind| !self.covers(**kind)) {
            return Err(RelayInfoError::KindUncovered(*kind));
        }
        Ok(())
    }
}

/// The NIP-11 URL of a relay: its WebSocket URL with the scheme mapped
/// (`wss` → `https`, `ws` → `http`); a URL of another scheme is left as is.
#[must_use]
pub fn info_url(relay_url: &str) -> String {
    if let Some(rest) = relay_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = relay_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        relay_url.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn reads_the_reference_relays_document() {
        let info = RelayInfo::from_json(
            r#"{"name":"relay.sanki.app","supported_nips":[1,11],"software":"https://github.com/hoytech/strfry",
                "limitation":{"created_at_lower_limit":1,"created_at_upper_limit":5,
                "created_at_window_kinds":[3418,3419,3420,3422,3423,3425],"min_pow_difficulty":10}}"#,
        )
        .unwrap();
        assert_eq!(info.name.as_deref(), Some("relay.sanki.app"));
        assert_eq!(info.past_tolerance(), Some(1));
        assert_eq!(info.future_tolerance(), 5);
        assert_eq!(info.min_pow_difficulty(), 10);
        assert!(info.covers(3423));
        assert!(!info.covers(0));
        assert_eq!(info.check_self_timed(), Ok(()));
    }

    #[test]
    fn absent_members_read_as_the_profile_says() {
        let info = RelayInfo::from_json(r#"{"limitation":{"created_at_lower_limit":0}}"#).unwrap();
        assert_eq!(info.min_pow_difficulty(), 0);
        assert_eq!(info.future_tolerance(), DEFAULT_FUTURE_TOLERANCE);
        assert!(info.covers(3423) && info.covers(0));
        assert_eq!(info.check_self_timed(), Ok(()));
        // No limitation at all.
        let bare = RelayInfo::from_json(r#"{"name":"x"}"#).unwrap();
        assert_eq!(
            bare.check_self_timed(),
            Err(RelayInfoError::PastBoundMissing)
        );
    }

    #[test]
    fn refuses_a_loose_or_partial_window() {
        let loose =
            RelayInfo::from_json(r#"{"limitation":{"created_at_lower_limit":60}}"#).unwrap();
        assert_eq!(
            loose.check_self_timed(),
            Err(RelayInfoError::PastBoundLoose(60))
        );
        let partial = RelayInfo::from_json(
            r#"{"limitation":{"created_at_lower_limit":1,"created_at_window_kinds":[3422,3423,3425]}}"#,
        )
        .unwrap();
        assert_eq!(
            partial.check_self_timed(),
            Err(RelayInfoError::KindUncovered(3420))
        );
    }

    #[test]
    fn a_member_of_the_wrong_type_reads_as_absent() {
        let info = RelayInfo::from_json(
            r#"{"limitation":{"created_at_lower_limit":"1","min_pow_difficulty":-3,"created_at_window_kinds":"all"}}"#,
        )
        .unwrap();
        assert_eq!(info.past_tolerance(), None);
        assert_eq!(info.min_pow_difficulty(), 0);
        assert_eq!(info.limitation.created_at_window_kinds, None);
        assert!(RelayInfo::from_json("[]").is_err());
    }

    #[test]
    fn maps_the_relay_url_to_its_document() {
        assert_eq!(info_url("wss://relay.sanki.app"), "https://relay.sanki.app");
        assert_eq!(info_url("ws://127.0.0.1:7777"), "http://127.0.0.1:7777");
        assert_eq!(info_url("https://x"), "https://x");
    }
}
