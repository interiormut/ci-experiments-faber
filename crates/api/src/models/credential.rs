use chrono::{DateTime, Utc};
use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::credentials;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = credentials)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Credential {
    pub id: Uuid,
    pub user_id: Uuid,
    pub label: String,
    pub key_ciphertext: Vec<u8>,
    pub key_nonce: Vec<u8>,
    pub key_version: String,
    pub last_four: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = credentials)]
pub struct NewCredential<'a> {
    pub id: Uuid,
    pub user_id: Uuid,
    pub label: &'a str,
    pub key_ciphertext: &'a [u8],
    pub key_nonce: &'a [u8],
    pub key_version: &'a str,
    pub last_four: &'a str,
}
