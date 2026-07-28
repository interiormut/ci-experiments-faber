//! Failure as ordinary values.
//!
//! Nothing here is retried, backed off, or repaired. Retry policy is the
//! harness's; this crate reports what the provider said and stops.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The request never got an answer: DNS, TLS, connect, timeout, or a
    /// connection dropped mid-stream.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The provider reported a failure: a non-success status, or an error
    /// frame delivered mid-stream (in which case `status` is `None`).
    #[error("provider error{}{}: {message}",
        .status.map(|s| format!(" {s}")).unwrap_or_default(),
        .kind.as_deref().map(|k| format!(" ({k})")).unwrap_or_default())]
    Api {
        status: Option<u16>,
        /// The provider's own error classification, when it gives one
        /// (`rate_limit_error`, `overloaded_error`, `invalid_request_error`).
        kind: Option<String>,
        message: String,
        /// The provider's request id, for reporting the failure upstream.
        request_id: Option<String>,
    },

    /// A response body — or one SSE frame of it — didn't parse.
    #[error("could not decode provider response: {0}")]
    Decode(String),

    /// The stream ended without the call ever starting. Distinct from a turn
    /// that produced no content: nothing was said, so nothing can be recorded.
    #[error("provider closed the stream without sending a response")]
    EmptyResponse,

    /// A tool call's streamed arguments were not valid JSON.
    #[error("tool `{tool}` produced malformed input JSON: {source}")]
    ToolInput {
        tool: String,
        source: serde_json::Error,
    },

    /// A thread was called while its last turn was still the assistant's.
    ///
    /// Providers reject a trailing assistant turn, and appending to one would
    /// mean this crate inventing a turn the caller didn't write.
    #[error(
        "thread ends with an assistant turn; append a user turn or tool results before calling again"
    )]
    ExpectedUserTurn,
}

impl Error {
    /// Whether the provider classified this as a transient condition.
    ///
    /// Informational only — deciding whether to retry is the harness's job.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Transport(source) => source.is_timeout() || source.is_connect(),
            Error::Api { status, kind, .. } => {
                matches!(status, Some(429) | Some(500..=599))
                    || matches!(
                        kind.as_deref(),
                        Some("rate_limit_error" | "overloaded_error" | "api_error")
                    )
            }
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
