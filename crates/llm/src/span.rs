//! Spans: provider-rendered prefix bytes, spliced back into a later request
//! rather than reconstructed from a value tree.
//!
//! H5 is the reason this module exists: type stability does not give byte
//! stability, and round-tripping history through a parsed value loses
//! information no middle layer can recover (key order, integer precision,
//! unschematized `raw`/`extra` content). A [`RenderedSpan`] is the fix —
//! it names bytes a wire module already produced and keeps them, so a later
//! render can splice them in unchanged instead of asking a provider's SDK to
//! re-emit the same conversation and hoping it matches.

use std::collections::BTreeMap;

use crate::error::Error;
use crate::types::{Message, ReasoningHistory, Turn};

/// Provider-rendered bytes for a conversation prefix, captured at render
/// time.
///
/// Scoped to the `(provider, model, reasoning)` it was rendered for. Reusing
/// it against a different scope is a typed refusal ([`Error::SpanScope`] or
/// [`Error::SpanReasoning`]) rather than a silent fall back to sending the
/// content by value — the alternative is exactly the failure the design calls
/// unacceptable: expensive, silent, and attributable to nothing.
///
/// `reasoning` is part of the scope because the policy decides what the bytes
/// contain: a prefix rendered while reasoning was sent back carries thinking
/// blocks the current policy would have dropped, and splicing it under the new
/// policy would send a history that is neither one thing nor the other.
#[derive(Clone, Debug)]
pub struct RenderedSpan {
    pub provider: String,
    pub model: String,
    pub reasoning: ReasoningHistory,
    /// Region name -> that region's rendered array elements, comma-joined,
    /// *without* the enclosing brackets (e.g. `"system"`, `"messages"`).
    /// Splicing wraps a region in `[` `]` and inserts it directly; the bytes
    /// are never parsed back into a value.
    pub regions: BTreeMap<String, Vec<u8>>,
}

/// The bytes and reusable prefix produced by rendering a [`Request`](crate::Request).
#[derive(Clone, Debug)]
pub struct RenderedRequest {
    /// The finished JSON body, ready to send verbatim.
    pub body: Vec<u8>,
    /// This call's rendered prefix: the exact regions a later call can
    /// splice back in to reproduce these bytes unchanged.
    pub prefix: RenderedSpan,
}

/// Splits `turns` into an optional leading span (valid only at index 0) and
/// the plain messages that follow.
///
/// A present span is checked against `provider`/`model` here, once, so every
/// wire module gets the refusal for free rather than reimplementing it.
pub(crate) fn split_span<'a>(
    turns: &'a [Turn],
    provider: &str,
    model: &str,
    reasoning: ReasoningHistory,
) -> Result<(Option<&'a RenderedSpan>, Vec<&'a Message>), Error> {
    let mut span = None;
    let mut rest = Vec::with_capacity(turns.len());

    for (index, turn) in turns.iter().enumerate() {
        match turn {
            Turn::Span(candidate) => {
                if index != 0 {
                    return Err(Error::SpanPosition { index });
                }
                if candidate.provider != provider || candidate.model != model {
                    return Err(Error::SpanScope {
                        span_provider: candidate.provider.clone(),
                        span_model: candidate.model.clone(),
                        request_provider: provider.to_owned(),
                        request_model: model.to_owned(),
                    });
                }
                if candidate.reasoning != reasoning {
                    return Err(Error::SpanReasoning {
                        span: candidate.reasoning,
                        request: reasoning,
                    });
                }
                span = Some(candidate);
            }
            Turn::Value(message) => rest.push(message),
        }
    }

    Ok((span, rest))
}

/// Appends a freshly-serialized element to a region, inserting the
/// separating comma only when the region already holds something.
///
/// Only for *new* content a wire module is rendering for the first time —
/// never for bytes recovered from a span, which are spliced as-is.
pub(crate) fn append_element(region: &mut Vec<u8>, value: &serde_json::Value) {
    if !region.is_empty() {
        region.push(b',');
    }
    region.extend_from_slice(&serde_json::to_vec(value).expect("Value always serializes"));
}

/// Wraps a region's joined elements in `[` `]`, as a string ready to embed
/// verbatim via [`serde_json::value::RawValue`].
pub(crate) fn wrap_array(bytes: &[u8]) -> Result<String, Error> {
    let inner = std::str::from_utf8(bytes)
        .map_err(|error| Error::Decode(format!("rendered region is not valid utf-8: {error}")))?;
    Ok(format!("[{inner}]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    const FULL: ReasoningHistory = ReasoningHistory::Full;

    #[test]
    fn a_span_anywhere_but_first_is_rejected() {
        let turns = vec![
            Turn::Value(Message::user("hi")),
            Turn::Span(RenderedSpan {
                provider: "anthropic".into(),
                model: "m".into(),
                reasoning: FULL,
                regions: BTreeMap::new(),
            }),
        ];
        let error = split_span(&turns, "anthropic", "m", FULL).unwrap_err();
        assert!(matches!(error, Error::SpanPosition { index: 1 }));
    }

    #[test]
    fn a_span_from_another_scope_is_rejected() {
        let turns = vec![Turn::Span(RenderedSpan {
            provider: "anthropic".into(),
            model: "claude".into(),
            reasoning: FULL,
            regions: BTreeMap::new(),
        })];
        let error = split_span(&turns, "openai", "gpt", FULL).unwrap_err();
        assert!(matches!(error, Error::SpanScope { .. }));
    }

    #[test]
    fn a_span_rendered_under_another_reasoning_policy_is_rejected() {
        let turns = vec![Turn::Span(RenderedSpan {
            provider: "anthropic".into(),
            model: "claude".into(),
            reasoning: ReasoningHistory::Full,
            regions: BTreeMap::new(),
        })];
        let error =
            split_span(&turns, "anthropic", "claude", ReasoningHistory::Omitted).unwrap_err();
        assert!(matches!(
            error,
            Error::SpanReasoning {
                span: ReasoningHistory::Full,
                request: ReasoningHistory::Omitted,
            }
        ));
    }
}
