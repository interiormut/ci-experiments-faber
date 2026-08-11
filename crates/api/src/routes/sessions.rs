use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use diesel::{ExpressionMethods, JoinOnDsl, QueryDsl, SelectableHelper};
use diesel_async::{AsyncConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    access::{authorize_session, authorize_workspace, personal_workspace},
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        now_epoch,
        session::{NewSession, Session, UpdateSession},
        thread::{NewThread, Thread},
    },
    routes::{clamp_limit, deserialize_optional_field, threads::{ThreadResponse, thread_response}},
    schema::{session, thread, workspace_member},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sessions", get(list).post(create))
        .route(
            "/api/sessions/{id}",
            get(get_session).patch(update).delete(remove),
        )
        .route(
            "/api/sessions/{id}/threads",
            get(list_threads).post(create_thread),
        )
}

const MAX_TITLE_CHARS: usize = 200;

#[derive(Deserialize)]
struct ListQuery {
    workspace_id: Option<Uuid>,
    limit: Option<i64>,
}

/// Both fields optional: `POST /api/sessions` with `{}` is the intended default path.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateRequest {
    workspace_id: Option<Uuid>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct UpdateRequest {
    /// `null` clears the title; omitting the key leaves it unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    title: Option<Option<String>>,
    /// `true` stamps `closed_at`; `false` reopens.
    closed: Option<bool>,
}

#[derive(Deserialize)]
struct CreateThreadRequest {
    /// Fork source. Must be given together with `forked_at_seq` — the database `CHECK`
    /// requires both or neither.
    parent_id: Option<Uuid>,
    forked_at_seq: Option<i32>,
}

#[derive(Serialize)]
struct SessionResponse {
    id: Uuid,
    workspace_id: Uuid,
    title: Option<String>,
    created_at: i64,
    closed_at: Option<i64>,
}

fn session_response(s: &Session) -> SessionResponse {
    SessionResponse {
        id: s.id,
        workspace_id: s.workspace_id,
        title: s.title.clone(),
        created_at: s.created_at,
        closed_at: s.closed_at,
    }
}

/// A new session always arrives with its root thread — a session with no thread is not a
/// state any caller has a use for.
#[derive(Serialize)]
struct CreatedSessionResponse {
    #[serde(flatten)]
    session: SessionResponse,
    root_thread: ThreadResponse,
}

fn validate_title(title: &str) -> Result<(), AppError> {
    if title.is_empty() {
        return Err(AppError::BadRequest("title cannot be empty".into()));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(AppError::BadRequest(format!(
            "title must be {MAX_TITLE_CHARS} characters or fewer"
        )));
    }
    Ok(())
}

/// Sessions across every workspace the caller belongs to, newest first, optionally
/// narrowed to one workspace.
async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(params): Query<ListQuery>,
) -> ApiResult<Json<Vec<SessionResponse>>> {
    let mut conn = state.db.get().await?;

    if let Some(workspace_id) = params.workspace_id {
        authorize_workspace(&mut conn, user.id, workspace_id).await?;
    }

    let mut query = session::table
        .inner_join(
            workspace_member::table.on(workspace_member::workspace_id.eq(session::workspace_id)),
        )
        .filter(workspace_member::user_id.eq(user.id))
        .into_boxed();

    if let Some(workspace_id) = params.workspace_id {
        query = query.filter(session::workspace_id.eq(workspace_id));
    }

    let rows: Vec<Session> = query
        .order(session::created_at.desc())
        .limit(clamp_limit(params.limit))
        .select(Session::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.list"))?;

    Ok(Json(rows.iter().map(session_response).collect()))
}

/// Creates a session and its root thread atomically. With no body fields the session
/// lands in the caller's personal workspace and is untitled.
async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    body: Option<Json<CreateRequest>>,
) -> ApiResult<(StatusCode, Json<CreatedSessionResponse>)> {
    let input = body.map(|Json(input)| input).unwrap_or_default();

    let title = input.title.as_deref().map(str::trim);
    if let Some(title) = title {
        validate_title(title)?;
    }

    let mut conn = state.db.get().await?;

    let workspace = match input.workspace_id {
        Some(id) => authorize_workspace(&mut conn, user.id, id).await?,
        None => personal_workspace(&mut conn, user.id).await?,
    };

    let now = now_epoch();
    let session_id = Uuid::now_v7();
    let thread_id = Uuid::now_v7();

    let (created_session, root_thread) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                let created_session: Session = diesel::insert_into(session::table)
                    .values(&NewSession {
                        id: session_id,
                        workspace_id: workspace.id,
                        title,
                        created_at: now,
                    })
                    .returning(Session::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(|err| AppError::db(err, "sessions.create.insert_session"))?;

                let root_thread: Thread = diesel::insert_into(thread::table)
                    .values(&NewThread {
                        id: thread_id,
                        session_id,
                        parent_id: None,
                        forked_at_seq: None,
                        created_at: now,
                    })
                    .returning(Thread::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(|err| AppError::db(err, "sessions.create.insert_root_thread"))?;

                Ok((created_session, root_thread))
            }
            .scope_boxed()
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedSessionResponse {
            session: session_response(&created_session),
            root_thread: thread_response(&root_thread),
        }),
    ))
}

async fn get_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SessionResponse>> {
    let mut conn = state.db.get().await?;
    let found = authorize_session(&mut conn, user.id, id).await?;
    Ok(Json(session_response(&found)))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<SessionResponse>> {
    let title = input
        .title
        .as_ref()
        .map(|opt| opt.as_deref().map(str::trim));

    if let Some(Some(title)) = title {
        validate_title(title)?;
    }

    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let patch = UpdateSession {
        title,
        closed_at: input
            .closed
            .map(|closed| closed.then(now_epoch)),
    };

    let updated: Session = diesel::update(session::table.filter(session::id.eq(id)))
        .set(patch)
        .returning(Session::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::NotFound => AppError::NotFound,
            other => AppError::db(other, "sessions.update"),
        })?;

    Ok(Json(session_response(&updated)))
}

/// Deletes the session and, by cascade, every thread, run, exchange, and transcript row
/// under it — including ground truth that is otherwise append-only. The append-only
/// triggers permit this because they test whether the *parent* row still exists, and in a
/// cascade it does not. `PATCH { "closed": true }` is the reversible alternative.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let deleted = diesel::delete(session::table.filter(session::id.eq(id)))
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn list_threads(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ThreadResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let rows: Vec<Thread> = thread::table
        .filter(thread::session_id.eq(id))
        .order(thread::created_at.asc())
        .select(Thread::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.list_threads"))?;

    Ok(Json(rows.iter().map(thread_response).collect()))
}

/// Creates a thread in the session — a root thread by default, or a fork when
/// `parent_id` and `forked_at_seq` are both supplied.
async fn create_thread(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CreateThreadRequest>>,
) -> ApiResult<(StatusCode, Json<ThreadResponse>)> {
    let input = body.map(|Json(input)| input).unwrap_or(CreateThreadRequest {
        parent_id: None,
        forked_at_seq: None,
    });

    // Rejected here rather than at the database `CHECK` so the caller gets a 400 naming
    // the problem instead of a 500 naming a constraint.
    if input.parent_id.is_some() != input.forked_at_seq.is_some() {
        return Err(AppError::BadRequest(
            "parent_id and forked_at_seq must be given together".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    if let (Some(parent_id), Some(forked_at_seq)) = (input.parent_id, input.forked_at_seq) {
        let parent: Thread = thread::table
            .filter(thread::id.eq(parent_id))
            .filter(thread::session_id.eq(id))
            .select(Thread::as_select())
            .first(&mut conn)
            .await
            .map_err(|err| match err {
                diesel::result::Error::NotFound => {
                    AppError::BadRequest("parent thread not found in this session".into())
                }
                other => AppError::db(other, "sessions.create_thread.load_parent"),
            })?;

        // `forked_at_seq` is inclusive, so it must name a position the parent has
        // actually allocated.
        if parent.next_seq == 0 {
            return Err(AppError::BadRequest(
                "parent thread has no history to fork from".into(),
            ));
        }
        if forked_at_seq < 0 || forked_at_seq >= parent.next_seq {
            return Err(AppError::BadRequest(format!(
                "forked_at_seq must be between 0 and {}",
                parent.next_seq - 1
            )));
        }
    }

    let created: Thread = diesel::insert_into(thread::table)
        .values(&NewThread {
            id: Uuid::now_v7(),
            session_id: id,
            parent_id: input.parent_id,
            forked_at_seq: input.forked_at_seq,
            created_at: now_epoch(),
        })
        .returning(Thread::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.create_thread"))?;

    Ok((StatusCode::CREATED, Json(thread_response(&created))))
}
