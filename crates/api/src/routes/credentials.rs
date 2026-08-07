use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, post},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    crypto::encrypt_key,
    error::{ApiResult, AppError},
    models::credential::{Credential, NewCredential},
    schema::credentials,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/credentials", post(create).get(list))
        .route("/api/credentials/:id", delete(remove))
}

#[derive(Deserialize)]
struct CreateRequest {
    label: String,
    key: String,
}

#[derive(Serialize)]
struct CredentialResponse {
    id: Uuid,
    label: String,
    last_four: String,
    created_at: DateTime<Utc>,
}

fn credential_response(c: &Credential) -> CredentialResponse {
    CredentialResponse {
        id: c.id,
        label: c.label.clone(),
        last_four: c.last_four.clone(),
        created_at: c.created_at,
    }
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<CredentialResponse>)> {
    let label = input.label.trim();
    if label.is_empty() {
        return Err(AppError::BadRequest("label is required".into()));
    }
    if label.chars().count() > 100 {
        return Err(AppError::BadRequest("label must be 100 characters or fewer".into()));
    }

    let key = input.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest("key is required".into()));
    }

    let last_four: String = key.chars().rev().take(4).collect::<String>().chars().rev().collect();

    let cred_id = Uuid::now_v7();
    let (ciphertext, nonce) = encrypt_key(key.as_bytes(), state.master_key.as_bytes(), cred_id, user.id)
        .map_err(|_| AppError::Internal)?;

    let new_cred = NewCredential {
        id: cred_id,
        user_id: user.id,
        label,
        key_ciphertext: &ciphertext,
        key_nonce: &nonce,
        key_version: "v1",
        last_four: &last_four,
    };

    let mut conn = state.db.get().await?;

    let inserted: Credential = diesel::insert_into(credentials::table)
        .values(&new_cred)
        .returning(Credential::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a credential named '{label}' already exists")),
            other => AppError::db(other, "credentials.create"),
        })?;

    Ok((StatusCode::CREATED, Json(credential_response(&inserted))))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<CredentialResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<Credential> = credentials::table
        .filter(credentials::user_id.eq(user.id))
        .order(credentials::created_at.asc())
        .select(Credential::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "credentials.list"))?;

    Ok(Json(rows.iter().map(credential_response).collect()))
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        credentials::table
            .filter(credentials::id.eq(id))
            .filter(credentials::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "credentials.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}
