//! Provider-neutral request and content types.
//!
//! Nothing here is Anthropic-shaped on purpose: a provider module owns the
//! translation to and from its own wire format. The one concession to reality
//! is `ToolDef::input_schema`, which is JSON Schema because every provider
//! that supports tools speaks it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who authored a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One turn in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// Concatenation of every text block, ignoring thinking and tool traffic.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A piece of a message.
///
/// `Unknown` exists so a provider that grows a block type we don't model yet
/// round-trips it rather than dropping it — the contract may break across
/// major versions, but a block that arrives must be one that can be sent back.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Model reasoning. `signature` is opaque and must be preserved verbatim
    /// when the block is sent back to the same model.
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Unknown {
        raw: Value,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// A tool the model may call. Core carries the declaration; dispatch belongs
/// to whatever the workflow handed the harness.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

/// Reasoning configuration. Providers that don't reason ignore it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Thinking {
    /// The model decides when and how much to think.
    Adaptive {
        display: ThinkingDisplay,
    },
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    /// Thinking blocks arrive with empty text.
    Omitted,
    /// Thinking blocks carry a readable summary.
    Summarized,
}

/// How much the model should spend on a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// Sampling knobs, all optional.
///
/// Current Anthropic models reject non-default `temperature`/`top_p`/`top_k`
/// outright, so a `None` here means "omit the field", not "send the default".
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Sampling {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
}

impl Sampling {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.top_p.is_none() && self.top_k.is_none()
    }
}

/// A single model call.
///
/// `model` is a plain string: roles are an indirection the workflow resolves
/// before anything reaches this crate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    pub thinking: Option<Thinking>,
    pub effort: Option<Effort>,
    pub sampling: Sampling,
    pub stop_sequences: Vec<String>,
    /// Fields merged into the request body verbatim, for provider features
    /// this crate has no opinion about yet. Keys that collide with fields
    /// above are ignored.
    pub extra: serde_json::Map<String, Value>,
}

/// Enough headroom that a normal answer isn't truncated; a harness that cares
/// sets its own.
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;

impl Request {
    /// The zero-configuration call: a model and a conversation.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system: None,
            messages,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            effort: None,
            sampling: Sampling::default(),
            stop_sequences: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

/// Why the model stopped.
///
/// Surfaced, never acted on: resuming a `PauseTurn` or falling back on a
/// `Refusal` is a decision about the shape of the loop, which is the harness's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    ContextWindowExceeded,
    Other(String),
}

/// Detail attached to a refusal. Absent for every other stop reason.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StopDetails {
    pub category: Option<String>,
    pub explanation: Option<String>,
}

/// Token accounting for one call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    /// Applies a partial report, overwriting exactly the fields it carries.
    ///
    /// A stream reports input counts up front and output counts at the end,
    /// so a call's totals are assembled from several partial reports. A
    /// reported zero overwrites like any other value — a refusal declined
    /// before any output really did produce zero output tokens, and recording
    /// that as "unreported" would be a lie about what happened.
    pub fn apply(&mut self, delta: &UsageDelta) {
        if let Some(count) = delta.input_tokens {
            self.input_tokens = count;
        }
        if let Some(count) = delta.output_tokens {
            self.output_tokens = count;
        }
        if let Some(count) = delta.cache_read_input_tokens {
            self.cache_read_input_tokens = count;
        }
        if let Some(count) = delta.cache_creation_input_tokens {
            self.cache_creation_input_tokens = count;
        }
    }
}

/// One provider report of token usage. `None` means the field was absent from
/// that report, which is not the same as it being zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDelta {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

impl From<Usage> for UsageDelta {
    fn from(usage: Usage) -> Self {
        Self {
            input_tokens: Some(usage.input_tokens),
            output_tokens: Some(usage.output_tokens),
            cache_read_input_tokens: Some(usage.cache_read_input_tokens),
            cache_creation_input_tokens: Some(usage.cache_creation_input_tokens),
        }
    }
}
