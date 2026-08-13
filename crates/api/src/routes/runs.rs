use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    access::authorize_run,
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::transcript::Transcript,
    routes::clamp_limit,
    run as runner,
    schema::{run, transcript},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/runs/{id}/transcript", get(list_transcript))
        .route("/api/runs/{id}/interrupt", post(interrupt))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    /// Return only events strictly after this `seq`, so a client can fetch the tail
    /// without refetching the run. This is the durable record behind
    /// `GET /api/sessions/{id}/stream`, and where a subscriber that fell behind
    /// re-syncs from.
    after_seq: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct TranscriptResponse {
    id: Uuid,
    seq: i64,
    /// Free-form event tag. Deliberately not an enum in the database
    /// (`history-abstract.md` H8.7) so a new variant is not a migration.
    kind: String,
    payload: Value,
    created_at: i64,
}

fn transcript_response(t: &Transcript) -> TranscriptResponse {
    TranscriptResponse {
        id: t.id,
        seq: t.seq,
        kind: t.kind.clone(),
        payload: t.payload.clone(),
        created_at: t.created_at,
    }
}

/// What the user saw, in order — the harness-yielded event stream, not the provider
/// exchange. The two are separate logs and neither derives the other
/// (`history-abstract.md` H2); ground truth is not exported here.
async fn list_transcript(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<TranscriptQuery>,
) -> ApiResult<Json<Vec<TranscriptResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_run(&mut conn, user.id, id).await?;

    let mut query = transcript::table
        .filter(transcript::run_id.eq(id))
        .into_boxed();

    if let Some(after_seq) = params.after_seq {
        query = query.filter(transcript::seq.gt(after_seq));
    }

    let rows: Vec<Transcript> = query
        .order(transcript::seq.asc())
        .limit(clamp_limit(params.limit))
        .select(Transcript::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "runs.list_transcript"))?;

    Ok(Json(rows.iter().map(transcript_response).collect()))
}

/// Asks a run in progress to stop, and returns as soon as the ask has landed.
///
/// Deliberately not a promise that the run has ended by the time this
/// responds. Stopping is a signal the harness meets as an ordinary failure of
/// its next model call or tool invocation (`abstract.md`: "cancellation
/// surfaces as an ordinary failure the harness has to deal with"), so a harness
/// that wants to commit what it has, or say one last thing, gets to — and a
/// harness that ignores the signal entirely is killed once the grace period is
/// out. Either way the end of the run is announced where every other end is,
/// on `GET /api/sessions/{id}/stream`, as `run_interrupted`.
///
/// Sending it twice is not an error: the flag is a state, not an edge, and a
/// user pressing stop again because nothing visibly happened yet is asking for
/// something reasonable.
async fn interrupt(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    authorize_run(&mut conn, user.id, id).await?;

    if runner::raise_interrupt(&state.interrupts, id) {
        return Ok(StatusCode::ACCEPTED);
    }

    // Re-read rather than trusting the row loaded a moment ago: a run that
    // finished between the two is the common way to reach here, and the whole
    // job left is telling "already over" apart from "not ours", which the
    // stale row would get wrong in exactly that case.
    let completed_at: Option<i64> = run::table
        .filter(run::id.eq(id))
        .select(run::completed_at)
        .first(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::NotFound => AppError::NotFound,
            other => AppError::db(other, "runs.interrupt.reload"),
        })?;

    if completed_at.is_some() {
        return Err(AppError::Conflict("run has already finished".into()));
    }

    // Running, but not here. The registry is in-process (`run.rs`), so an
    // instance that does not own the run has no way to reach it — reported
    // rather than answered with a misleading 202, which would tell the user
    // their run is stopping when nothing received the ask.
    tracing::warn!(run_id = %id, "interrupt for a run this instance does not own");
    Err(AppError::ServiceUnavailable(
        "run is not in progress on this instance".into(),
    ))
}
