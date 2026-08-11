use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::{workspace, workspace_member};

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = workspace)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Workspace {
    pub id: Uuid,
    pub kind: String,
    pub user_id: Option<Uuid>,
    pub created_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = workspace)]
pub struct NewWorkspace {
    pub id: Uuid,
    pub kind: String,
    pub user_id: Option<Uuid>,
    pub created_at: i64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = workspace_member)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub joined_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = workspace_member)]
pub struct NewWorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub joined_at: i64,
}
