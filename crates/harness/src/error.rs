//! Errors surfaced across the ops boundary.
//!
//! `types.d.ts:296` (`HarnessError`) is explicit: a harness that has to regex
//! an error message to decide whether to retry has been handed nothing. So
//! `kind`/`transient`/`status`/`requestId` are attached as *properties* on
//! the thrown JS `Error` — not flattened into the message string.
//!
//! Implemented by hand rather than via `#[derive(JsError)]`: the derive's
//! per-field `#[property]` support converts through `&self.field`, which
//! `PropertyValue` only has `From` impls for on a handful of reference types
//! (`&str`, `&f64`, `&i32`, `&u32` — no `&String`). Our fields are owned,
//! dynamic strings, so a manual `JsErrorClass` is less code than routing
//! around that than fighting it.

use std::borrow::Cow;

use deno_error::{AdditionalProperties, JsErrorClass, PropertyValue};

use crate::mapping::HarnessErrorInfo;

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    /// A model call failed. Carries [`HarnessErrorInfo`] as JS properties.
    #[error("{message}")]
    Model {
        kind: &'static str,
        message: String,
        transient: bool,
        status: Option<u16>,
        request_id: Option<String>,
    },

    #[error("unknown stream handle {rid}")]
    UnknownStream { rid: u32 },

    #[error(
        "stream {rid} is already being polled elsewhere; a single Call cannot be iterated \
         and read via `.completion` concurrently"
    )]
    StreamBusy { rid: u32 },

    #[error("no such tool `{name}`")]
    UnknownTool { name: String },

    #[error("tool `{name}` could not be dispatched: {message}")]
    ToolDispatchFailed { name: String, message: String },

    #[error("commit was not granted to this harness")]
    CommitNotGranted,

    #[error("unknown harness function `{name}`")]
    UnknownFunction { name: String },

    #[error("harness function `{name}` failed: {message}")]
    FunctionFailed { name: String, message: String },
}

impl JsErrorClass for OpError {
    fn get_class(&self) -> Cow<'static, str> {
        Cow::Borrowed(match self {
            OpError::Model { .. } => "Error",
            OpError::UnknownStream { .. } => "RangeError",
            OpError::StreamBusy { .. } => "Error",
            OpError::UnknownTool { .. } => "RangeError",
            OpError::ToolDispatchFailed { .. } => "Error",
            OpError::CommitNotGranted => "Error",
            OpError::UnknownFunction { .. } => "RangeError",
            OpError::FunctionFailed { .. } => "Error",
        })
    }

    fn get_message(&self) -> Cow<'static, str> {
        Cow::Owned(self.to_string())
    }

    fn get_additional_properties(&self) -> AdditionalProperties {
        let OpError::Model {
            kind,
            transient,
            status,
            request_id,
            ..
        } = self
        else {
            return Box::new(std::iter::empty());
        };

        let properties: Vec<(Cow<'static, str>, PropertyValue)> = vec![
            (Cow::Borrowed("kind"), PropertyValue::from(*kind)),
            (
                Cow::Borrowed("transient"),
                PropertyValue::from(transient.to_string()),
            ),
            (
                Cow::Borrowed("status"),
                match status {
                    Some(status) => PropertyValue::Number(f64::from(*status)),
                    None => PropertyValue::from(String::new()),
                },
            ),
            (
                Cow::Borrowed("requestId"),
                PropertyValue::from(request_id.clone().unwrap_or_default()),
            ),
        ];
        Box::new(properties.into_iter())
    }

    fn get_ref(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }
}

impl From<HarnessErrorInfo> for OpError {
    fn from(info: HarnessErrorInfo) -> Self {
        OpError::Model {
            kind: info.kind,
            message: info.message,
            transient: info.transient,
            status: info.status,
            request_id: info.request_id,
        }
    }
}

impl From<&llm::Error> for OpError {
    fn from(error: &llm::Error) -> Self {
        HarnessErrorInfo::from(error).into()
    }
}

impl From<llm::Error> for OpError {
    fn from(error: llm::Error) -> Self {
        OpError::from(&error)
    }
}

impl From<&crate::validate::Refusal> for OpError {
    fn from(refusal: &crate::validate::Refusal) -> Self {
        HarnessErrorInfo::from(refusal).into()
    }
}

impl From<crate::validate::Refusal> for OpError {
    fn from(refusal: crate::validate::Refusal) -> Self {
        OpError::from(&refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_scope_error_carries_its_kind_as_a_property_not_the_message() {
        let error: OpError = llm::Error::SpanScope {
            span_provider: "anthropic".into(),
            span_model: "claude".into(),
            request_provider: "openai".into(),
            request_model: "gpt".into(),
        }
        .into();
        let properties: Vec<_> = error.get_additional_properties().collect();
        assert!(properties.iter().any(|(name, value)| {
            name == "kind" && matches!(value, PropertyValue::String(s) if s == "span_scope")
        }));
    }
}
