use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::span;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = span)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Span {
    pub id: Uuid,
    pub exchange_id: Uuid,
    pub start_offset: i64,
    pub end_offset: i64,
    pub created_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = span)]
pub struct NewSpan {
    pub id: Uuid,
    pub exchange_id: Uuid,
    pub start_offset: i64,
    pub end_offset: i64,
    pub created_at: i64,
}
