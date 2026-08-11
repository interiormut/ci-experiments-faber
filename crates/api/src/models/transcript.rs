use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::transcript;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = transcript)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Transcript {
    pub id: Uuid,
    /// Selected as part of the row; routes reach transcript rows through the run they
    /// already hold, so it is not echoed back.
    #[allow(dead_code)]
    pub run_id: Uuid,
    pub seq: i64,
    pub kind: String,
    pub payload: Value,
    pub created_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = transcript)]
pub struct NewTranscript<'a> {
    pub id: Uuid,
    pub run_id: Uuid,
    pub seq: i64,
    pub kind: &'a str,
    pub payload: Value,
    pub created_at: i64,
}
