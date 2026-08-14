//! Folding a run's streamed transcript events into the messages they add up
//! to, for storage.
//!
//! A model turn crosses `op_yield` as a `message_start`, a `block_start` per
//! content block, one `block_delta` per token-ish fragment, and the matching
//! stops — hundreds of events for one paragraph. Streaming them live is the
//! point; *storing* every one of them is not. The `transcript` table then
//! holds a row per delta, and every replay (`GET /api/runs/{id}/transcript`)
//! re-sends the whole delta stream to a client that only ever renders the
//! concatenation.
//!
//! So this is a retention policy, not a rewrite: the deltas are still
//! published live, and what gets *persisted* is the single `message` event
//! they fold into. H2 still holds literally — the transcript remains exactly
//! what the harness yielded, minus the intermediate states of content the
//! harness also yielded whole. Nothing here reads the provider stream, and
//! nothing Core observed at the capability boundary is touched:
//! `exchange.provider_events_digest` keeps the provider events as received,
//! which is what makes it an audit record.
//!
//! The fold is over the *harness-facing* wire vocabulary
//! (`crates/harness/src/mapping.rs`'s `LLMEvent`, camelCase), not over
//! `llm::Event` — those types are `Serialize`-only by design, and what
//! reaches this layer is the JSON that crossed the isolate boundary. The
//! shape it produces matches `mapping::Message`, so a client reads a
//! persisted `message` the same way it reads the `input` event.

use serde_json::{Map, Value, json};

/// The compacted event's `kind`: one whole message the harness streamed.
pub const KIND_MESSAGE: &str = "message";

/// What the caller should do with one streamed event.
#[derive(Debug, Default)]
pub struct Folded {
    /// A message left open by earlier events that *this* event closes.
    /// Publish it before the event itself, so it keeps its place in `seq`
    /// order.
    pub flushed: Option<Value>,
    /// Whether the raw event is itself durable. False for everything the
    /// fold absorbs.
    pub persist_raw: bool,
    /// The message this event completed. Publish it after the event.
    pub completed: Option<Value>,
}

/// Folds streamed model events into whole messages.
///
/// One per run. Events outside the model vocabulary (`tool_result`, and
/// anything a harness invents — `kind` is free-form, H8.7) pass through
/// untouched, and close an open message first rather than being ordered
/// ahead of content that preceded them.
#[derive(Debug, Default)]
pub struct Compactor {
    open: Option<OpenMessage>,
}

#[derive(Debug, Default)]
struct OpenMessage {
    blocks: Vec<PartialBlock>,
    stop_reason: Option<Value>,
}

#[derive(Debug)]
enum PartialBlock {
    Text(String),
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
    Unknown(Value),
    /// An index the stream never opened. A hole to drop, not a block to
    /// invent — same rule `llm::Accumulator` applies.
    Missing,
}

impl Compactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one yielded event.
    pub fn push(&mut self, kind: &str, payload: &Value) -> Folded {
        match kind {
            "message_start" => Folded {
                // A provider that never closed the previous message is not a
                // reason to lose it.
                flushed: self.close(),
                persist_raw: false,
                completed: None,
            },
            "block_start" => {
                self.block_start(payload);
                Folded::default()
            }
            "block_delta" => {
                self.block_delta(payload);
                Folded::default()
            }
            // Nothing to record: a block is complete once its deltas are in,
            // and a tool call's arguments are parsed when the message closes.
            "block_stop" => Folded::default(),
            "message_delta" => {
                if let Some(open) = self.open.as_mut()
                    && let Some(reason) = payload.get("stopReason")
                {
                    open.stop_reason = Some(reason.clone());
                }
                Folded::default()
            }
            "message_stop" => Folded {
                flushed: None,
                persist_raw: false,
                completed: self.close(),
            },
            // A provider frame with no neutral equivalent, passed through raw
            // (`llm::Event::Unknown`). Durable, because nothing here can tell
            // whether it mattered — but *inside* the message, not after it: a
            // mid-stream frame this crate doesn't model must not chop one
            // generation into several stored messages.
            "unknown" => Folded {
                flushed: None,
                persist_raw: true,
                completed: None,
            },
            _ => Folded {
                flushed: self.close(),
                persist_raw: true,
                completed: None,
            },
        }
    }

    /// Closes whatever is still open when the run's stream ends.
    ///
    /// A harness that stops mid-message — an error, a `break` out of
    /// `for await` — still yielded the text it yielded, and it is durable.
    pub fn finish(&mut self) -> Option<Value> {
        self.close()
    }

    fn block_start(&mut self, payload: &Value) {
        let Some(index) = index_of(payload) else {
            return;
        };
        let block = payload.get("block").unwrap_or(&Value::Null);
        let partial = match block.get("type").and_then(Value::as_str) {
            Some("text") => PartialBlock::Text(String::new()),
            Some("thinking") => PartialBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
            Some("tool_use") => PartialBlock::ToolUse {
                id: string_at(block, "id"),
                name: string_at(block, "name"),
                json: String::new(),
            },
            _ => PartialBlock::Unknown(block.get("raw").cloned().unwrap_or_else(|| block.clone())),
        };

        // A `block_start` with no `message_start` ahead of it still carries
        // content; open a message for it rather than dropping the block.
        let open = self.open.get_or_insert_with(OpenMessage::default);
        if index >= open.blocks.len() {
            open.blocks.resize_with(index, || PartialBlock::Missing);
            open.blocks.push(partial);
        } else {
            open.blocks[index] = partial;
        }
    }

    fn block_delta(&mut self, payload: &Value) {
        let Some(index) = index_of(payload) else {
            return;
        };
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let Some(block) = open.blocks.get_mut(index) else {
            return;
        };
        let delta = payload.get("delta").unwrap_or(&Value::Null);
        match (block, delta.get("type").and_then(Value::as_str)) {
            (PartialBlock::Text(text), Some("text")) => text.push_str(&string_at(delta, "text")),
            (PartialBlock::Thinking { thinking, .. }, Some("thinking")) => {
                thinking.push_str(&string_at(delta, "thinking"))
            }
            (PartialBlock::Thinking { signature, .. }, Some("thinking_signature")) => {
                *signature = Some(string_at(delta, "signature"))
            }
            (PartialBlock::ToolUse { json, .. }, Some("tool_input_json")) => {
                json.push_str(&string_at(delta, "partialJson"))
            }
            _ => {}
        }
    }

    /// Renders the open message, if there is one with anything in it.
    fn close(&mut self) -> Option<Value> {
        let open = self.open.take()?;

        let content: Vec<Value> = open
            .blocks
            .into_iter()
            .filter_map(|block| match block {
                PartialBlock::Missing => None,
                PartialBlock::Text(text) => Some(json!({ "type": "text", "text": text })),
                PartialBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    let mut object = Map::new();
                    object.insert("type".into(), json!("thinking"));
                    object.insert("thinking".into(), json!(thinking));
                    if let Some(signature) = signature {
                        object.insert("signature".into(), json!(signature));
                    }
                    Some(Value::Object(object))
                }
                PartialBlock::ToolUse { id, name, json } => Some(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": parse_arguments(&json),
                })),
                PartialBlock::Unknown(raw) => Some(json!({ "type": "unknown", "raw": raw })),
            })
            .collect();

        // A message that opened and closed with no content at all is not
        // something to show anyone.
        if content.is_empty() && open.stop_reason.is_none() {
            return None;
        }

        let mut message = Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert("content".into(), Value::Array(content));
        if let Some(reason) = open.stop_reason {
            message.insert("stopReason".into(), reason);
        }
        Some(Value::Object(message))
    }
}

/// A tool call's streamed arguments, parsed if they can be.
///
/// Never an error: a stream cut off mid-JSON has already cost the caller the
/// tool call, and failing the insert here would cost them the whole message
/// as well. The unparseable text is kept verbatim under `raw`, which is what
/// the client does with the same case on the live path.
fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({ "raw": raw }))
}

fn index_of(payload: &Value) -> Option<usize> {
    payload
        .get("index")
        .and_then(Value::as_u64)
        .map(|index| index as usize)
}

fn string_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a compactor over a whole event list, returning what would be
    /// persisted — `(kind, payload)` in publication order.
    fn persisted(events: &[(&str, Value)]) -> Vec<(String, Value)> {
        let mut compactor = Compactor::new();
        let mut out = Vec::new();
        for (kind, payload) in events {
            let folded = compactor.push(kind, payload);
            if let Some(message) = folded.flushed {
                out.push((KIND_MESSAGE.to_owned(), message));
            }
            if folded.persist_raw {
                out.push(((*kind).to_owned(), payload.clone()));
            }
            if let Some(message) = folded.completed {
                out.push((KIND_MESSAGE.to_owned(), message));
            }
        }
        if let Some(message) = compactor.finish() {
            out.push((KIND_MESSAGE.to_owned(), message));
        }
        out
    }

    fn text_message(fragments: &[&str]) -> Vec<(&'static str, Value)> {
        let mut events = vec![
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "text"}}),
            ),
        ];
        for fragment in fragments {
            events.push((
                "block_delta",
                json!({"index": 0, "delta": {"type": "text", "text": fragment}}),
            ));
        }
        events.push(("block_stop", json!({"index": 0})));
        events.push((
            "message_delta",
            json!({"stopReason": {"type": "end_turn"}, "usage": {}}),
        ));
        events.push(("message_stop", json!({})));
        events
    }

    /// The wire shape as the harness boundary actually serializes it — the
    /// only fixture in this module that isn't hand-written, and the one that
    /// keeps the rest honest about camelCase. `LLMEvent` is `Serialize`-only,
    /// so this is how the vocabulary gets pinned to the compiler.
    fn wire(event: llm::Event) -> (String, Value) {
        let value = serde_json::to_value(harness::mapping::LLMEvent::from(event))
            .expect("an LLMEvent always serializes");
        let kind = value["type"].as_str().expect("tagged").to_owned();
        (kind, value)
    }

    #[test]
    fn it_folds_the_vocabulary_the_boundary_really_emits() {
        let events = [
            wire(llm::Event::MessageStart {
                id: "m".into(),
                model: "x".into(),
                usage: llm::UsageDelta::default(),
            }),
            wire(llm::Event::BlockStart {
                index: 0,
                block: llm::BlockStart::Text,
            }),
            wire(llm::Event::BlockDelta {
                index: 0,
                delta: llm::Delta::Text {
                    content: "hi".into(),
                },
            }),
            wire(llm::Event::BlockStart {
                index: 1,
                block: llm::BlockStart::ToolUse {
                    id: "t1".into(),
                    name: "grep".into(),
                },
            }),
            wire(llm::Event::BlockDelta {
                index: 1,
                delta: llm::Delta::ToolInputJson {
                    content: "{\"q\":\"x\"}".into(),
                },
            }),
            wire(llm::Event::MessageDelta {
                stop_reason: Some(llm::StopReason::ToolUse),
                stop_details: None,
                usage: llm::UsageDelta::default(),
            }),
            wire(llm::Event::MessageStop),
        ];

        let borrowed: Vec<(&str, Value)> = events
            .iter()
            .map(|(kind, payload)| (kind.as_str(), payload.clone()))
            .collect();
        let rows = persisted(&borrowed);

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1["content"],
            json!([
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "t1", "name": "grep", "input": {"q": "x"}},
            ])
        );
        assert_eq!(rows[0].1["stopReason"], json!({"type": "tool_use"}));
    }

    #[test]
    fn an_unmodelled_provider_frame_does_not_split_the_message() {
        // `Event::Unknown` is a mid-stream frame `crates/llm` has no neutral
        // shape for. Closing the message around it would store one generation
        // as several.
        let mut events = vec![
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "text"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "text", "text": "one "}}),
            ),
        ];
        let unknown = wire(llm::Event::Unknown {
            raw: json!({"type": "something_new"}),
        });
        assert_eq!(unknown.0, "unknown", "the tag this branch matches on");
        events.push(("unknown", unknown.1));
        events.push((
            "block_delta",
            json!({"index": 0, "delta": {"type": "text", "text": "message"}}),
        ));
        events.push(("message_stop", json!({})));

        let rows = persisted(&events);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "unknown", "kept, and kept where it arrived");
        assert_eq!(
            rows[1].1["content"],
            json!([{"type": "text", "text": "one message"}])
        );
    }

    #[test]
    fn a_streamed_message_persists_as_one_event() {
        let rows = persisted(&text_message(&["Hel", "lo, ", "world"]));

        assert_eq!(rows.len(), 1, "only the folded message is durable");
        let (kind, payload) = &rows[0];
        assert_eq!(kind, KIND_MESSAGE);
        assert_eq!(payload["role"], json!("assistant"));
        assert_eq!(
            payload["content"],
            json!([{"type": "text", "text": "Hello, world"}])
        );
        assert_eq!(payload["stopReason"], json!({"type": "end_turn"}));
    }

    #[test]
    fn tool_arguments_fold_into_the_call_they_belong_to() {
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "tool_use", "id": "t1", "name": "grep"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "tool_input_json", "partialJson": "{\"q\":"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "tool_input_json", "partialJson": "\"hi\"}"}}),
            ),
            ("block_stop", json!({"index": 0})),
            ("message_stop", json!({})),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1["content"],
            json!([{"type": "tool_use", "id": "t1", "name": "grep", "input": {"q": "hi"}}])
        );
    }

    #[test]
    fn arguments_cut_off_mid_json_keep_their_text_instead_of_failing() {
        // Losing the tool call is already the cost of a truncated stream;
        // losing the message it was part of would be a second one.
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "tool_use", "id": "t1", "name": "grep"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "tool_input_json", "partialJson": "{\"q\":"}}),
            ),
        ]);

        assert_eq!(rows.len(), 1, "the unterminated message is still flushed");
        assert_eq!(rows[0].1["content"][0]["input"], json!({"raw": "{\"q\":"}));
    }

    #[test]
    fn an_empty_argument_call_gets_an_empty_object() {
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "tool_use", "id": "t1", "name": "now"}}),
            ),
            ("block_stop", json!({"index": 0})),
            ("message_stop", json!({})),
        ]);

        assert_eq!(rows[0].1["content"][0]["input"], json!({}));
    }

    #[test]
    fn a_tool_result_stays_a_row_of_its_own() {
        let mut events = text_message(&["calling"]);
        events.push((
            "tool_result",
            json!({"type": "tool_result", "toolUseId": "t1", "content": "ok", "isError": false}),
        ));
        let rows = persisted(&events);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, KIND_MESSAGE);
        assert_eq!(rows[1].0, "tool_result");
    }

    #[test]
    fn an_event_outside_the_model_vocabulary_closes_the_message_ahead_of_it() {
        // Ordering, not tidiness: a compacted message published *after* an
        // event that followed it would replay out of order.
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "text"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "text", "text": "hi"}}),
            ),
            (
                "note",
                json!({"type": "note", "text": "something the harness invented"}),
            ),
        ]);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, KIND_MESSAGE);
        assert_eq!(
            rows[0].1["content"],
            json!([{"type": "text", "text": "hi"}])
        );
        assert_eq!(rows[1].0, "note");
    }

    #[test]
    fn two_messages_in_one_run_stay_two_events() {
        let mut events = text_message(&["first"]);
        events.extend(text_message(&["second"]));
        let rows = persisted(&events);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1["content"][0]["text"], json!("first"));
        assert_eq!(rows[1].1["content"][0]["text"], json!("second"));
    }

    #[test]
    fn a_message_left_open_by_a_broken_stream_is_still_persisted() {
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "text"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "text", "text": "half a th"}}),
            ),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1["content"],
            json!([{"type": "text", "text": "half a th"}])
        );
    }

    #[test]
    fn a_skipped_index_leaves_no_block_behind() {
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 2, "block": {"type": "text"}}),
            ),
            (
                "block_delta",
                json!({"index": 2, "delta": {"type": "text", "text": "hi"}}),
            ),
            ("message_stop", json!({})),
        ]);

        assert_eq!(
            rows[0].1["content"],
            json!([{"type": "text", "text": "hi"}])
        );
    }

    #[test]
    fn thinking_keeps_its_signature() {
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            (
                "block_start",
                json!({"index": 0, "block": {"type": "thinking"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "thinking", "thinking": "hmm"}}),
            ),
            (
                "block_delta",
                json!({"index": 0, "delta": {"type": "thinking_signature", "signature": "sig"}}),
            ),
            ("message_stop", json!({})),
        ]);

        assert_eq!(
            rows[0].1["content"],
            json!([{"type": "thinking", "thinking": "hmm", "signature": "sig"}])
        );
    }

    #[test]
    fn a_message_that_carried_nothing_is_not_recorded() {
        let rows = persisted(&[
            (
                "message_start",
                json!({"id": "m", "model": "x", "usage": {}}),
            ),
            ("message_stop", json!({})),
        ]);

        assert!(rows.is_empty());
    }
}
