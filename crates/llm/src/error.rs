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

    /// A [`crate::Turn::Span`] was rendered against one provider/model and
    /// reused against another.
    ///
    /// The tempting handling is a silent fall back to sending the span's
    /// content by value. That is exactly the failure the design calls out as
    /// unacceptable: expensive, silent, and attributable to nothing. This is
    /// a typed refusal instead.
    #[error(
        "span was rendered for {span_provider}/{span_model}, cannot be reused against {request_provider}/{request_model}"
    )]
    SpanScope {
        span_provider: String,
        span_model: String,
        request_provider: String,
        request_model: String,
    },

    /// A [`crate::Turn::Span`] was rendered under a different reasoning-history
    /// policy than the request replaying it.
    ///
    /// Refused for the same reason as [`Self::SpanScope`]: the span's bytes
    /// already committed to what reasoning history contains, and splicing them
    /// under a changed policy sends neither the old history nor the new one.
    #[error(
        "span was rendered with reasoning history {span:?}, cannot be reused against {request:?}"
    )]
    SpanReasoning {
        span: crate::types::ReasoningHistory,
        request: crate::types::ReasoningHistory,
    },

    /// A [`crate::Turn::Span`] appeared somewhere other than index 0.
    ///
    /// A span names a *prefix*; only the tail may be sent by value, so a span
    /// anywhere but the front of the array is not expressible.
    #[error("a span turn may only appear as the first message; found at index {index}")]
    SpanPosition { index: usize },
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
