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
}
