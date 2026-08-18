use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schema::presentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamHostMode {
    #[default]
    Loopback,
    Preserve,
}

impl UpstreamHostMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Preserve => "preserve",
        }
    }
}

#[derive(Clone, Debug, Queryable, Selectable)]
#[diesel(table_name = presentation)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Presentation {
    pub id: Uuid,
    pub session_id: Uuid,
    pub environment_label: String,
    pub port: i32,
    pub token: String,
    pub upstream_host_mode: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Insertable)]
#[diesel(table_name = presentation)]
pub struct NewPresentation<'a> {
    pub id: Uuid,
    pub session_id: Uuid,
    pub environment_label: &'a str,
    pub port: i32,
    pub token: &'a str,
    pub upstream_host_mode: &'a str,
}
