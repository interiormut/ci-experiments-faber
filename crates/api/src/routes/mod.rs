mod credentials;
mod models;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    auth::{AuthUser, extract_token},
    error::{ApiResult, AppError},
    models::user::{UpdateUserProfile, User},
    schema::users::dsl::users,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/api/me", get(me).patch(update_me))
        .route("/api/logout", post(logout))
        .merge(credentials::router())
        .merge(models::router())
}

/// Revokes the caller's Surge session (best-effort) and clears the `surge_session` cookie
/// for the configured cookie domain. Unauthenticated calls are a no-op success — logout
/// should never itself require being logged in.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(raw_token) = extract_token(&headers)
        && let Some(token) = surge::SessionToken::from_raw(&raw_token)
        && let Err(error) = state.auth.revoke_session(&token).await
    {
        tracing::warn!(error = %error, "failed to revoke surge session during logout");
    }

    let clear_cookie = format!(
        "surge_session=; Domain={}; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax",
        state.config.surge_cookie_domain
    );

    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, clear_cookie)])
}

#[derive(Serialize)]
struct MeResponse {
    id: String,
    username: String,
    display_name: String,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct UpdateMeRequest {
    display_name: Option<String>,
    avatar_url: Option<Option<String>>,
}

async fn me(State(_state): State<AppState>, AuthUser(user): AuthUser) -> Json<MeResponse> {
    Json(me_response(&user))
}

async fn update_me(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<UpdateMeRequest>,
) -> ApiResult<Json<MeResponse>> {
    let mut conn = state.db.get().await?;

    if let Some(display_name) = input.display_name.as_deref() {
        validate_display_name(display_name.trim())?;
    }

    let patch = UpdateUserProfile {
        display_name: input.display_name.as_deref().map(str::trim),
        avatar_url: input.avatar_url.as_ref().map(|value| value.as_deref()),
    };

    let updated = diesel::update(users.filter(crate::schema::users::id.eq(user.id)))
        .set((
            patch,
            crate::schema::users::updated_at.eq(chrono::Utc::now()),
        ))
        .returning(User::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "routes.update_me.update_user_profile"))?;

    Ok(Json(me_response(&updated)))
}

fn me_response(user: &User) -> MeResponse {
    MeResponse {
        id: user.id.to_string(),
        username: user.username.clone(),
        display_name: if user.display_name.trim().is_empty() {
            user.username.clone()
        } else {
            user.display_name.clone()
        },
        avatar_url: user.avatar_url.clone(),
    }
}

fn validate_display_name(value: &str) -> Result<(), AppError> {
    if value.is_empty() {
        return Err(AppError::BadRequest("display_name is required".into()));
    }
    if value.chars().count() > 80 {
        return Err(AppError::BadRequest(
            "display_name must be 80 characters or fewer".into(),
        ));
    }
    Ok(())
}
