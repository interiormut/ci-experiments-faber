use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::exchange;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = exchange)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Exchange {
    pub id: Uuid,
    pub run_id: Uuid,
    pub request_blob_digest: Vec<u8>,
    pub provider_events_digest: Option<Vec<u8>>,
    pub usage: Option<Value>,
    pub outcome: Option<Value>,
    pub expected_cache_tokens: i64,
    pub actual_cache_tokens: Option<i64>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = exchange)]
pub struct NewExchange<'a> {
    pub id: Uuid,
    pub run_id: Uuid,
    pub request_blob_digest: &'a [u8],
    pub expected_cache_tokens: i64,
    pub started_at: i64,
}
