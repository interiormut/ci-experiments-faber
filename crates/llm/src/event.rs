//! The streaming event vocabulary, and the accumulator that folds it back
//! into a message.
//!
//! Core records what happened; it does not decide what a turn is. Every event
//! a provider emits is surfaced, including ones this crate doesn't model
//! (`Event::Unknown`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;
use crate::types::{ContentBlock, Message, StopDetails, StopReason, Usage, UsageDelta};

/// One unit of progress from a model call.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    MessageStart {
        id: String,
        model: String,
        usage: UsageDelta,
    },
    BlockStart {
        index: usize,
        block: BlockStart,
    },
    BlockDelta {
        index: usize,
        delta: Delta,
    },
    BlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: Option<StopReason>,
        stop_details: Option<StopDetails>,
        usage: UsageDelta,
    },
    MessageStop,
    /// A provider event with no neutral equivalent, passed through raw.
    Unknown {
        raw: Value,
    },
}

/// The opening of a content block, before any text has arrived.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockStart {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
    Unknown { raw: Value },
}

/// An incremental update to an open block.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    Text(String),
    Thinking(String),
    /// The opaque signature that closes a thinking block.
    ThinkingSignature(String),
    /// A fragment of the JSON encoding of a tool call's arguments.
    ToolInputJson(String),
    Unknown {
        raw: Value,
    },
}

/// The result of a completed model call.
#[derive(Clone, Debug)]
pub struct Completion {
    pub id: String,
    pub model: String,
    pub message: Message,
    pub stop_reason: Option<StopReason>,
    /// Populated only when `stop_reason` is [`StopReason::Refusal`].
    pub stop_details: Option<StopDetails>,
    pub usage: Usage,
}

impl Completion {
    /// The assistant's visible text.
    pub fn text(&self) -> String {
        self.message.text()
    }

    /// The tool calls the model asked for, in order.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.message.content.iter().filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

/// Folds a stream of [`Event`]s into a [`Completion`].
///
/// Kept separate from the stream so a harness can render deltas as they land
/// and still recover the whole message at the end.
#[derive(Debug, Default)]
pub struct Accumulator {
    id: String,
    model: String,
    blocks: Vec<PartialBlock>,
    stop_reason: Option<StopReason>,
    stop_details: Option<StopDetails>,
    usage: Usage,
    started: bool,
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
}

impl Accumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: &Event) {
        match event {
            Event::MessageStart { id, model, usage } => {
                self.started = true;
                self.id = id.clone();
                self.model = model.clone();
                self.usage.apply(usage);
            }
            Event::BlockStart { index, block } => {
                let partial = match block {
                    BlockStart::Text => PartialBlock::Text(String::new()),
                    BlockStart::Thinking => PartialBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                    BlockStart::ToolUse { id, name } => PartialBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        json: String::new(),
                    },
                    BlockStart::Unknown { raw } => PartialBlock::Unknown(raw.clone()),
                };
                if *index >= self.blocks.len() {
                    self.blocks
                        .resize_with(*index, || PartialBlock::Unknown(Value::Null));
                    self.blocks.push(partial);
                } else {
                    self.blocks[*index] = partial;
                }
            }
            Event::BlockDelta { index, delta } => {
                let Some(block) = self.blocks.get_mut(*index) else {
                    return;
                };
                match (block, delta) {
                    (PartialBlock::Text(text), Delta::Text(chunk)) => text.push_str(chunk),
                    (PartialBlock::Thinking { thinking, .. }, Delta::Thinking(chunk)) => {
                        thinking.push_str(chunk)
                    }
                    (PartialBlock::Thinking { signature, .. }, Delta::ThinkingSignature(chunk)) => {
                        *signature = Some(chunk.clone())
                    }
                    (PartialBlock::ToolUse { json, .. }, Delta::ToolInputJson(chunk)) => {
                        json.push_str(chunk)
                    }
                    _ => {}
                }
            }
            Event::MessageDelta {
                stop_reason,
                stop_details,
                usage,
            } => {
                if stop_reason.is_some() {
                    self.stop_reason = stop_reason.clone();
                }
                if stop_details.is_some() {
                    self.stop_details = stop_details.clone();
                }
                self.usage.apply(usage);
            }
            Event::BlockStop { .. } | Event::MessageStop | Event::Unknown { .. } => {}
        }
    }

    /// Consumes the accumulated state.
    ///
    /// Fails if the call never started — a connection that opened and closed
    /// without a single event is a lost turn, not an empty answer — or if a
    /// tool call's streamed arguments don't parse as JSON, which means the
    /// call cannot be dispatched.
    pub fn finish(self) -> Result<Completion, Error> {
        if !self.started {
            return Err(Error::EmptyResponse);
        }

        let content = self
            .blocks
            .into_iter()
            .map(|block| match block {
                PartialBlock::Text(text) => Ok(ContentBlock::Text { text }),
                PartialBlock::Thinking {
                    thinking,
                    signature,
                } => Ok(ContentBlock::Thinking {
                    thinking,
                    signature,
                }),
                PartialBlock::ToolUse { id, name, json } => {
                    // An empty-argument tool call streams no delta at all.
                    let input = if json.trim().is_empty() {
                        Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(&json).map_err(|source| Error::ToolInput {
                            tool: name.clone(),
                            source,
                        })?
                    };
                    Ok(ContentBlock::ToolUse { id, name, input })
                }
                PartialBlock::Unknown(value) => Ok(ContentBlock::Unknown { raw: value }),
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(Completion {
            id: self.id,
            model: self.model,
            message: Message::assistant(content),
            stop_reason: self.stop_reason,
            stop_details: self.stop_details,
            usage: self.usage,
        })
    }
}
