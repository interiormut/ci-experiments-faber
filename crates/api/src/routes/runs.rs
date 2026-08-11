use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
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
    schema::transcript,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/runs/{id}/transcript", get(list_transcript))
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
