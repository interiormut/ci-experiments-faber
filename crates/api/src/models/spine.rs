use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::spine;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = spine)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Spine {
    pub thread_id: Uuid,
    pub seq: i64,
    pub exchange_id: Uuid,
    pub explicit_commit: bool,
    pub created_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = spine)]
pub struct NewSpine {
    pub thread_id: Uuid,
    pub seq: i64,
    pub exchange_id: Uuid,
    pub explicit_commit: bool,
    pub created_at: i64,
}
