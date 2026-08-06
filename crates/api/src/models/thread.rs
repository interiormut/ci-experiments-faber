use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::thread;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = thread)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Thread {
    pub id: Uuid,
    pub session_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub forked_at_seq: Option<i32>,
    pub next_seq: i32,
    pub created_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = thread)]
pub struct NewThread {
    pub id: Uuid,
    pub session_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub forked_at_seq: Option<i32>,
    pub created_at: i64,
}
