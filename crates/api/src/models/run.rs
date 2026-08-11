use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::run;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = run)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Run {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = run)]
pub struct NewRun {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub created_at: i64,
}
