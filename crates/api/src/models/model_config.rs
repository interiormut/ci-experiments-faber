use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::schema::models;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    Openai,
    Anthropic,
}

impl Wire {
    pub fn as_str(&self) -> &'static str {
        match self {
            Wire::Openai => "openai",
            Wire::Anthropic => "anthropic",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(Wire::Openai),
            "anthropic" => Some(Wire::Anthropic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = models)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ModelConfig {
    pub id: Uuid,
    pub user_id: Uuid,
    pub alias: String,
    pub base_url: String,
    pub wire: String,
    pub wire_id: String,
    pub family: Option<String>,
    pub credential_id: Option<Uuid>,
    pub params: Value,
    pub capabilities: Value,
    pub created_at: DateTime<Utc>,
}

/// The key under `capabilities` that carries the reasoning-history policy.
pub const REASONING_HISTORY_KEY: &str = "reasoning_history";

impl ModelConfig {
    /// How much of a replayed assistant turn's reasoning this model wants back.
    ///
    /// `None` means the row says nothing and the wire's own default applies —
    /// which is also the answer for a row written before this setting existed.
    /// The value is validated when the row is written (see `routes::models`),
    /// so anything unreadable here is a row that went around the API, and
    /// falling back to the wire default beats refusing to run.
    pub fn reasoning_history(&self) -> Option<llm::ReasoningHistory> {
        parse_reasoning_history(self.capabilities.get(REASONING_HISTORY_KEY)?).ok()?
    }
}

/// Reads a `capabilities.reasoning_history` value, naming what is wrong with
/// it rather than falling back — the write path wants the complaint.
pub fn parse_reasoning_history(value: &Value) -> Result<Option<llm::ReasoningHistory>, String> {
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| {
            format!("{REASONING_HISTORY_KEY} must be one of \"full\", \"text\", or \"omitted\"")
        })
}

#[derive(Insertable)]
#[diesel(table_name = models)]
pub struct NewModelConfig<'a> {
    pub id: Uuid,
    pub user_id: Uuid,
    pub alias: &'a str,
    pub base_url: &'a str,
    pub wire: &'a str,
    pub wire_id: &'a str,
    pub family: Option<&'a str>,
    pub credential_id: Option<Uuid>,
    pub params: Value,
    pub capabilities: Value,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = models)]
pub struct UpdateModelConfig<'a> {
    pub alias: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub wire: Option<&'a str>,
    pub wire_id: Option<&'a str>,
    pub family: Option<Option<&'a str>>,
    pub credential_id: Option<Option<Uuid>>,
    pub params: Option<Value>,
    pub capabilities: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ModelParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(flatten)]
    pub passthrough: serde_json::Map<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub context_window: u32,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub vision: bool,
    /// See [`ModelConfig::reasoning_history`] — that reader is the one the run
    /// path uses, since it has to answer for a row that carries nothing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_history: Option<llm::ReasoningHistory>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(capabilities: Value) -> ModelConfig {
        ModelConfig {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            alias: "fast".into(),
            base_url: "https://api.anthropic.com".into(),
            wire: "anthropic".into(),
            wire_id: "claude-opus-5".into(),
            family: None,
            credential_id: None,
            params: json!({}),
            capabilities,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_row_that_says_nothing_leaves_the_wire_its_default() {
        assert!(config(json!({})).reasoning_history().is_none());
        assert!(config(json!(null)).reasoning_history().is_none());
    }

    #[test]
    fn each_setting_reaches_the_run_as_itself() {
        for (written, expected) in [
            ("full", llm::ReasoningHistory::Full),
            ("text", llm::ReasoningHistory::Text),
            ("omitted", llm::ReasoningHistory::Omitted),
        ] {
            let config = config(json!({ REASONING_HISTORY_KEY: written }));
            assert_eq!(config.reasoning_history(), Some(expected));
        }
    }

    #[test]
    fn a_row_written_around_the_api_falls_back_rather_than_failing_the_run() {
        // The write path refuses this value; a row carrying it anyway came
        // from somewhere else, and the wire default beats refusing to run.
        let config = config(json!({ REASONING_HISTORY_KEY: "sometimes" }));
        assert!(config.reasoning_history().is_none());
    }

    #[test]
    fn the_write_path_gets_told_what_is_wrong_with_the_value() {
        assert!(parse_reasoning_history(&json!("sometimes")).is_err());
        assert!(parse_reasoning_history(&json!(true)).is_err());
        assert_eq!(parse_reasoning_history(&json!(null)), Ok(None));
    }

    #[test]
    fn other_capabilities_are_left_alone() {
        let config = config(json!({"context_window": 200000, REASONING_HISTORY_KEY: "text"}));
        assert_eq!(
            config.reasoning_history(),
            Some(llm::ReasoningHistory::Text)
        );
        assert_eq!(config.capabilities["context_window"], json!(200000));
    }
}
