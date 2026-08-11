use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::{session, session_ref};

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = session)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Session {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: Option<String>,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = session)]
pub struct NewSession<'a> {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: Option<&'a str>,
    pub created_at: i64,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = session)]
pub struct UpdateSession<'a> {
    /// `Some(None)` clears the title; `None` leaves it alone.
    pub title: Option<Option<&'a str>>,
    /// `Some(None)` reopens a closed session.
    pub closed_at: Option<Option<i64>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = session_ref)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SessionRef {
    pub token: String,
    pub session_id: Uuid,
    pub issued_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = session_ref)]
pub struct NewSessionRef<'a> {
    pub token: &'a str,
    pub session_id: Uuid,
    pub issued_at: i64,
}
