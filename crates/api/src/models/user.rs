use chrono::{DateTime, Utc};
use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::users;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: Uuid,
    pub identity_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = users)]
pub struct NewUser<'a> {
    pub id: Uuid,
    pub identity_id: Uuid,
    pub username: &'a str,
    pub display_name: &'a str,
    pub avatar_url: Option<&'a str>,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = users)]
pub struct UpdateUserProfile<'a> {
    pub display_name: Option<&'a str>,
    pub avatar_url: Option<Option<&'a str>>,
}
