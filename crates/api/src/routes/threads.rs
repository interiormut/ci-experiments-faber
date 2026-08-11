use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    access::authorize_thread,
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{run::Run, spine::Spine, thread::Thread},
    schema::{run, spine},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/threads/{id}", get(get_thread))
        .route("/api/threads/{id}/spine", get(list_spine))
        .route("/api/threads/{id}/runs", get(list_runs))
}

#[derive(Serialize)]
pub(super) struct ThreadResponse {
    pub id: Uuid,
    pub session_id: Uuid,
    /// Set on a fork, together with `forked_at_seq`.
    pub parent_id: Option<Uuid>,
    pub forked_at_seq: Option<i32>,
    /// Core-owned sequence allocator — the next `spine.seq` this thread will hand out.
    pub next_seq: i32,
    pub created_at: i64,
}

pub(super) fn thread_response(t: &Thread) -> ThreadResponse {
    ThreadResponse {
        id: t.id,
        session_id: t.session_id,
        parent_id: t.parent_id,
        forked_at_seq: t.forked_at_seq,
        next_seq: t.next_seq,
        created_at: t.created_at,
    }
}

#[derive(Serialize)]
struct SpineResponse {
    seq: i64,
    exchange_id: Uuid,
    /// `true` when a best-of-N winner was committed deliberately, rather than the
    /// implicit last-request default (`history-abstract.md` H4).
    explicit_commit: bool,
    created_at: i64,
}

fn spine_response(s: &Spine) -> SpineResponse {
    SpineResponse {
        seq: s.seq,
        exchange_id: s.exchange_id,
        explicit_commit: s.explicit_commit,
        created_at: s.created_at,
    }
}

#[derive(Serialize)]
struct RunResponse {
    id: Uuid,
    thread_id: Uuid,
    created_at: i64,
    completed_at: Option<i64>,
}

fn run_response(r: &Run) -> RunResponse {
    RunResponse {
        id: r.id,
        thread_id: r.thread_id,
        created_at: r.created_at,
        completed_at: r.completed_at,
    }
}

async fn get_thread(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ThreadResponse>> {
    let mut conn = state.db.get().await?;
    let thread = authorize_thread(&mut conn, user.id, id).await?;
    Ok(Json(thread_response(&thread)))
}

/// The thread's canonical history chain: one exchange per position, in `seq` order.
///
/// Written by `crate::run` when a run commits — each position names the exchange whose
/// lineage seeds the next turn.
async fn list_spine(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<SpineResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_thread(&mut conn, user.id, id).await?;

    let rows: Vec<Spine> = spine::table
        .filter(spine::thread_id.eq(id))
        .order(spine::seq.asc())
        .select(Spine::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "threads.list_spine"))?;

    Ok(Json(rows.iter().map(spine_response).collect()))
}

async fn list_runs(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<RunResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_thread(&mut conn, user.id, id).await?;

    let rows: Vec<Run> = run::table
        .filter(run::thread_id.eq(id))
        .order(run::created_at.asc())
        .select(Run::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "threads.list_runs"))?;

    Ok(Json(rows.iter().map(run_response).collect()))
}
