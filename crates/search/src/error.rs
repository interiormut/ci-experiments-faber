//! Failure as ordinary values.
//!
//! Two questions get asked of a failure, and they are different questions:
//! [`Error::is_transient`] asks whether *this call* might work if repeated,
//! and [`Error::disqualifies_instance`] asks whether *this instance* is worth
//! keeping in a pool at all. A rate limit is transient and does not
//! disqualify; an instance that serves HTML for a JSON request is neither
//! transient nor ever going to change its mind.

use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The request never got an answer: DNS, TLS, connect, timeout, or a
    /// connection dropped mid-body.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The instance answered with a failure status.
    #[error("instance error {status}: {message}")]
    Api { status: u16, message: String },

    /// The instance's bot limiter turned the request away.
    ///
    /// Its own doing, not the upstream search engine's — SearXNG ships a
    /// limiter that answers plain HTTP clients with `429` by default.
    #[error("rate limited by instance{}", .retry_after.map(|d| format!(", retry after {}s", d.as_secs())).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },

    /// The instance does not serve the JSON API.
    ///
    /// SearXNG requires `search.formats: [json]` in `settings.yml` and most
    /// public instances leave it out, in which case a `format=json` request
    /// gets the HTML search page with a `200` — a success status carrying an
    /// unusable body — or a flat `403`. Both mean the same thing: this
    /// instance can never serve us, and no amount of waiting changes it.
    #[error("instance does not serve the JSON API ({reason})")]
    NoJsonApi { reason: String },

    /// A response was JSON, but not JSON this crate understands.
    #[error("could not decode instance response: {0}")]
    Decode(String),

    /// `searx.space` could not be fetched or made sense of.
    #[error("instance directory unavailable: {0}")]
    Directory(String),

    /// Every instance in the pool was rate-limited, busy, or disqualified.
    ///
    /// Distinct from a query that simply found nothing: nothing was asked, so
    /// nothing can be reported.
    #[error("no usable instance available ({considered} considered, {verified} verified)")]
    NoInstance { considered: usize, verified: usize },

    /// A URL, timeout, or TLS setting was not usable.
    #[error("invalid configuration: {0}")]
    Config(String),
}

impl Error {
    /// Whether asking again could plausibly succeed.
    ///
    /// Informational. Nothing in this crate retries on the caller's behalf
    /// except [`crate::public::PublicSearxNg`]'s move to a *different*
    /// instance, which is a different act.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Transport(source) => {
                source.is_timeout() || source.is_connect() || source.is_request()
            }
            Error::Api { status, .. } => matches!(status, 500..=599 | 408 | 403),
            Error::RateLimited { .. } | Error::NoInstance { .. } => true,
            _ => false,
        }
    }

    /// Whether the instance turned us away for asking too often, rather than
    /// failing.
    ///
    /// Kept separate from [`is_transient`](Self::is_transient) because it
    /// carries an instruction: back off *this* instance for a while, and take
    /// the query elsewhere meanwhile. `403` is here because SearXNG's limiter
    /// answers with one about as readily as with `429`, and an instance that
    /// merely dislikes the shape of this request is not an instance to abandon.
    pub fn is_throttle(&self) -> bool {
        match self {
            Error::RateLimited { .. } => true,
            Error::Api { status, .. } => matches!(status, 403 | 429),
            _ => false,
        }
    }

    /// Whether the instance that produced this should be dropped from a pool
    /// permanently rather than merely cooled down.
    ///
    /// Deliberately, narrowly, one thing: an instance that served a `200` with
    /// a non-JSON body. That is the only unambiguous signal in the set —
    /// `search.formats` does not list `json`, and nothing we do will change it.
    ///
    /// Everything else stays in the pool. A pool that disqualifies on
    /// transport errors eats itself the first time the *local* network
    /// hiccups, and — measured, not guessed — a pool that disqualifies on the
    /// bot limiter's `429` burns roughly three quarters of the public network
    /// on first contact, which leaves nothing to distribute across.
    pub fn disqualifies_instance(&self) -> bool {
        match self {
            Error::NoJsonApi { .. } => true,
            // The instance saying this endpoint is not ours to call, in a way
            // no waiting will change. `403` is absent on purpose: see
            // `is_throttle`.
            Error::Api { status, .. } => matches!(status, 401 | 404 | 410),
            _ => false,
        }
    }

    /// How long the instance asked us to wait, when it said.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
