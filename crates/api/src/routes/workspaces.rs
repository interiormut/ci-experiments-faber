use axum::{Json, Router, extract::State, routing::get};
use diesel::{ExpressionMethods, JoinOnDsl, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::workspace::Workspace,
    schema::{workspace, workspace_member},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/workspaces", get(list))
}

#[derive(Serialize)]
struct WorkspaceResponse {
    id: Uuid,
    kind: String,
    /// Set only for a personal workspace; `common` workspaces have no owner.
    user_id: Option<Uuid>,
    created_at: i64,
}

fn workspace_response(w: &Workspace) -> WorkspaceResponse {
    WorkspaceResponse {
        id: w.id,
        kind: w.kind.clone(),
        user_id: w.user_id,
        created_at: w.created_at,
    }
}

/// Every workspace the caller is a member of. The personal one always appears — the
/// `users_create_workspace` trigger provisions it on first login.
async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<WorkspaceResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<Workspace> = workspace::table
        .inner_join(workspace_member::table.on(workspace_member::workspace_id.eq(workspace::id)))
        .filter(workspace_member::user_id.eq(user.id))
        .order(workspace::created_at.asc())
        .select(Workspace::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "workspaces.list"))?;

    Ok(Json(rows.iter().map(workspace_response).collect()))
}
