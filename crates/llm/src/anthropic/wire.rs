//! Translation between the neutral types and the Anthropic Messages API.
//!
//! Hand-written rather than derived: the neutral types' own `serde` impls are
//! for persistence, and a provider's wire format is free to drift away from
//! them.

use serde_json::{Map, Value, json};

use crate::error::Error;
use crate::event::{BlockStart, Delta, Event};
use crate::types::{
    ContentBlock, Effort, Message, Request, Role, StopDetails, StopReason, Thinking,
    ThinkingDisplay, ToolChoice, UsageDelta,
};

pub(super) fn request_body(request: &Request) -> Value {
    let mut body = Map::new();

    // Caller-supplied passthrough first, so modelled fields always win.
    for (key, value) in &request.extra {
        body.insert(key.clone(), value.clone());
    }

    body.insert("model".into(), json!(request.model));
    body.insert("max_tokens".into(), json!(request.max_tokens));
    body.insert("stream".into(), json!(true));
    body.insert(
        "messages".into(),
        Value::Array(request.messages.iter().map(message_to_json).collect()),
    );

    if let Some(system) = &request.system {
        body.insert("system".into(), json!(system));
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.input_schema,
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        body.insert(
            "tool_choice".into(),
            match choice {
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::Any => json!({"type": "any"}),
                ToolChoice::None => json!({"type": "none"}),
                ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
            },
        );
    }
    if let Some(thinking) = request.thinking {
        body.insert(
            "thinking".into(),
            match thinking {
                // `budget_tokens` is rejected on current models; adaptive is
                // the only on-mode. `display` defaults to omitted, which
                // yields thinking blocks with empty text.
                Thinking::Adaptive { display } => json!({
                    "type": "adaptive",
                    "display": match display {
                        ThinkingDisplay::Omitted => "omitted",
                        ThinkingDisplay::Summarized => "summarized",
                    },
                }),
                Thinking::Disabled => json!({"type": "disabled"}),
            },
        );
    }
    if let Some(effort) = request.effort {
        // Effort is nested under output_config, not top level.
        let level = match effort {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        };
        match body.get_mut("output_config") {
            Some(Value::Object(config)) => {
                config.insert("effort".into(), json!(level));
            }
            _ => {
                body.insert("output_config".into(), json!({ "effort": level }));
            }
        }
    }
    // Omitted when unset: current models reject these outright rather than
    // treating an explicit default as a no-op.
    if let Some(temperature) = request.sampling.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.sampling.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if let Some(top_k) = request.sampling.top_k {
        body.insert("top_k".into(), json!(top_k));
    }
    if !request.stop_sequences.is_empty() {
        body.insert("stop_sequences".into(), json!(request.stop_sequences));
    }

    Value::Object(body)
}

fn message_to_json(message: &Message) -> Value {
    json!({
        "role": match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        },
        "content": Value::Array(message.content.iter().map(block_to_json).collect()),
    })
}

fn block_to_json(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            let mut value = json!({"type": "thinking", "thinking": thinking});
            if let Some(signature) = signature {
                value["signature"] = json!(signature);
            }
            value
        }
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
        // Echoed back exactly as it arrived — see the Unknown note on ContentBlock.
        ContentBlock::Unknown { raw } => raw.clone(),
    }
}

/// Maps one SSE frame to a neutral event.
///
/// `Ok(None)` means the frame carries no information a caller needs (a
/// keepalive ping).
pub(super) fn parse_event(frame: &Value) -> Result<Option<Event>, Error> {
    let kind = frame
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Decode("stream frame has no `type`".into()))?;

    let event = match kind {
        "ping" => return Ok(None),
        "error" => {
            let error = frame.get("error");
            return Err(Error::Api {
                status: None,
                kind: error
                    .and_then(|error| error.get("type"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: error
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("stream reported an error")
                    .to_owned(),
                request_id: None,
            });
        }
        "message_start" => {
            let message = frame
                .get("message")
                .ok_or_else(|| Error::Decode("message_start has no `message`".into()))?;
            Event::MessageStart {
                id: string_at(message, "id"),
                model: string_at(message, "model"),
                usage: usage_from(message.get("usage")),
            }
        }
        "content_block_start" => Event::BlockStart {
            index: index_of(frame)?,
            block: block_start_from(frame.get("content_block")),
        },
        "content_block_delta" => Event::BlockDelta {
            index: index_of(frame)?,
            delta: delta_from(frame.get("delta")),
        },
        "content_block_stop" => Event::BlockStop {
            index: index_of(frame)?,
        },
        "message_delta" => {
            let delta = frame.get("delta");
            Event::MessageDelta {
                stop_reason: delta
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(stop_reason_from),
                // Populated only alongside a refusal.
                stop_details: delta
                    .and_then(|delta| delta.get("stop_details"))
                    .filter(|value| !value.is_null())
                    .map(|value| StopDetails {
                        category: value
                            .get("category")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        explanation: value
                            .get("explanation")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }),
                usage: usage_from(frame.get("usage")),
            }
        }
        "message_stop" => Event::MessageStop,
        _ => Event::Unknown { raw: frame.clone() },
    };

    Ok(Some(event))
}

fn index_of(frame: &Value) -> Result<usize, Error> {
    frame
        .get("index")
        .and_then(Value::as_u64)
        .map(|index| index as usize)
        .ok_or_else(|| Error::Decode("content block frame has no `index`".into()))
}

fn string_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn block_start_from(block: Option<&Value>) -> BlockStart {
    let Some(block) = block else {
        return BlockStart::Unknown { raw: Value::Null };
    };
    match block.get("type").and_then(Value::as_str) {
        Some("text") => BlockStart::Text,
        Some("thinking") => BlockStart::Thinking,
        Some("tool_use") => BlockStart::ToolUse {
            id: string_at(block, "id"),
            name: string_at(block, "name"),
        },
        _ => BlockStart::Unknown { raw: block.clone() },
    }
}

fn delta_from(delta: Option<&Value>) -> Delta {
    let Some(delta) = delta else {
        return Delta::Unknown { raw: Value::Null };
    };
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => Delta::Text(string_at(delta, "text")),
        Some("thinking_delta") => Delta::Thinking(string_at(delta, "thinking")),
        Some("signature_delta") => Delta::ThinkingSignature(string_at(delta, "signature")),
        Some("input_json_delta") => Delta::ToolInputJson(string_at(delta, "partial_json")),
        _ => Delta::Unknown { raw: delta.clone() },
    }
}

fn stop_reason_from(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        "model_context_window_exceeded" => StopReason::ContextWindowExceeded,
        other => StopReason::Other(other.to_owned()),
    }
}

/// Reads only the fields the report actually carries: an absent count and a
/// reported zero mean different things.
fn usage_from(usage: Option<&Value>) -> UsageDelta {
    let Some(usage) = usage else {
        return UsageDelta::default();
    };
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    UsageDelta {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
        cache_read_input_tokens: count("cache_read_input_tokens"),
        cache_creation_input_tokens: count("cache_creation_input_tokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Sampling;

    fn request() -> Request {
        Request::new("claude-opus-5", vec![Message::user("hi")])
    }

    #[test]
    fn bare_request_sends_nothing_it_was_not_given() {
        let body = request_body(&request());
        let object = body.as_object().unwrap();
        assert_eq!(object["model"], json!("claude-opus-5"));
        assert_eq!(object["stream"], json!(true));
        for absent in [
            "temperature",
            "top_p",
            "top_k",
            "thinking",
            "output_config",
            "tools",
            "tool_choice",
            "system",
            "stop_sequences",
        ] {
            assert!(
                !object.contains_key(absent),
                "unexpected `{absent}` in body"
            );
        }
    }

    #[test]
    fn effort_nests_under_output_config() {
        let mut request = request();
        request.effort = Some(Effort::XHigh);
        let body = request_body(&request);
        assert_eq!(body["output_config"]["effort"], json!("xhigh"));
        assert!(body.get("effort").is_none());
    }

    #[test]
    fn sampling_is_sent_only_when_set() {
        let mut request = request();
        request.sampling = Sampling {
            temperature: Some(0.5),
            ..Sampling::default()
        };
        let body = request_body(&request);
        assert_eq!(body["temperature"], json!(0.5));
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn modelled_fields_win_over_extra() {
        let mut request = request();
        request
            .extra
            .insert("model".into(), json!("something-else"));
        request
            .extra
            .insert("service_tier".into(), json!("standard"));
        let body = request_body(&request);
        assert_eq!(body["model"], json!("claude-opus-5"));
        assert_eq!(body["service_tier"], json!("standard"));
    }

    #[test]
    fn ping_carries_nothing() {
        assert!(parse_event(&json!({"type": "ping"})).unwrap().is_none());
    }

    #[test]
    fn unrecognised_frames_pass_through() {
        let event = parse_event(&json!({"type": "invented_later"}))
            .unwrap()
            .unwrap();
        assert!(matches!(event, Event::Unknown { .. }));
    }

    #[test]
    fn stream_error_frame_becomes_an_api_error() {
        let error = parse_event(&json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "overloaded"},
        }))
        .unwrap_err();
        assert!(error.is_transient(), "overload should read as transient");
    }

    #[test]
    fn refusal_carries_its_details() {
        let event = parse_event(&json!({
            "type": "message_delta",
            "delta": {"stop_reason": "refusal", "stop_details": {"category": "cyber"}},
            "usage": {"output_tokens": 12},
        }))
        .unwrap()
        .unwrap();
        match event {
            Event::MessageDelta {
                stop_reason,
                stop_details,
                usage,
            } => {
                assert_eq!(stop_reason, Some(StopReason::Refusal));
                assert_eq!(stop_details.unwrap().category.as_deref(), Some("cyber"));
                assert_eq!(usage.output_tokens, Some(12));
            }
            other => panic!("expected a message delta, got {other:?}"),
        }
    }
}
