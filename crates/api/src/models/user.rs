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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When this user was made an operator of the machines faber provides.
    /// `None` is the answer for everyone else.
    ///
    /// Nothing in the API writes it — a route that could grant it would be a
    /// privilege-escalation surface, and the first administrator has to be
    /// made out of band regardless. The migration that adds the column carries
    /// the statement that sets it.
    pub admin_since: Option<DateTime<Utc>>,
}

impl User {
    /// Whether this user may operate faber's own hosts.
    ///
    /// The one place the question is answered, so "who is an administrator" is
    /// a single grep rather than a predicate spread across handlers.
    pub fn is_admin(&self) -> bool {
        self.admin_since.is_some()
    }
}

#[derive(Insertable)]
#[diesel(table_name = users)]
pub struct NewUser {
    pub id: Uuid,
    pub identity_id: Uuid,
}
