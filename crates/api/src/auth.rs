use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use std::time::Instant;
use surge::{AuthRejection, AuthSession};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::user::{NewUser, User},
    state::AppState,
};

/// Verified Surge session identity from the incoming request.
#[derive(Debug, Clone)]
pub struct SurgeIdentity {
    pub identity_id: Uuid,
}

/// Reads the raw session token out of the `surge_session` cookie, falling back to a
/// `Bearer` token in `Authorization`. Mirrors `surge::extract::extract_token`.
pub(crate) fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(cookie_header) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for cookie in cookie_header.split(';') {
            let cookie = cookie.trim();
            if let Some(value) = cookie.strip_prefix("surge_session=") {
                return Some(value.to_owned());
            }
        }
    }

    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())?;
    auth_header.strip_prefix("Bearer ").map(str::to_owned)
}

impl FromRequestParts<AppState> for SurgeIdentity {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let started_at = Instant::now();

        let AuthSession(session) = AuthSession::from_request_parts(parts, state)
            .await
            .map_err(|rejection| match rejection {
                AuthRejection::Unauthorized(message) => AppError::Unauthorized(message),
                AuthRejection::ServiceUnavailable(message) => AppError::ServiceUnavailable(message),
            })?;

        tracing::info!(
            elapsed_ms = started_at.elapsed().as_millis(),
            username = %session.identity.username.as_str(),
            "surge identity extracted"
        );

        Ok(SurgeIdentity {
            identity_id: session.identity.id.into(),
        })
    }
}

impl SurgeIdentity {
    /// Returns the local `User` row, provisioning it on first login.
    /// Safe to call concurrently — uses ON CONFLICT DO NOTHING for idempotent inserts.
    pub async fn resolve_user(&self, state: &AppState) -> Result<User, AppError> {
        use crate::schema::users::dsl::*;

        let mut conn = state.db.get().await?;

        let existing: Option<User> = users
            .filter(identity_id.eq(self.identity_id))
            .select(User::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|err| AppError::db(err, "auth.resolve_user.lookup_user"))?;

        if let Some(u) = existing {
            return Ok(u);
        }

        let new_user = NewUser {
            id: Uuid::now_v7(),
            identity_id: self.identity_id,
        };

        // ON CONFLICT DO NOTHING handles concurrent first-logins racing to insert the same user.
        let inserted: Option<User> = diesel::insert_into(users)
            .values(&new_user)
            .on_conflict_do_nothing()
            .returning(User::as_returning())
            .get_result(&mut conn)
            .await
            .optional()
            .map_err(|err| AppError::db(err, "auth.resolve_user.insert_user"))?;

        let user = match inserted {
            Some(u) => u,
            None => users
                .filter(identity_id.eq(self.identity_id))
                .select(User::as_select())
                .first(&mut conn)
                .await
                .map_err(|err| AppError::db(err, "auth.resolve_user.lookup_raced_user"))?,
        };

        Ok(user)
    }
}

/// Fully provisioned authenticated user — use as an Axum extractor in protected handlers.
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let identity = SurgeIdentity::from_request_parts(parts, state).await?;
        let user = identity.resolve_user(state).await?;
        Ok(AuthUser(user))
    }
}

/// An authenticated user who administers faber's own hosts.
///
/// Read from `users.admin_since`, which no route writes: the flag is set by an
/// operator with a SQL statement, spelled out in the migration that adds the
/// column. That is the whole of the update path on purpose — a route able to
/// grant this would be a privilege-escalation surface, and the first
/// administrator has to be made out of band whatever else exists.
///
/// Refuses rather than hides. Everywhere else an unreachable row is a 404,
/// because whether someone else's session exists is itself information — but
/// there is nothing to conceal here: the administrative routes are the same
/// for every deployment, and a signed-in operator who has not been given the
/// flag is far better served by "you are not an administrator" than by a 404
/// that reads as a broken build.
pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let AuthUser(user) = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_admin() {
            tracing::warn!(user = %user.id, "refused an administrative request");
            return Err(AppError::Forbidden(
                "this account does not administer faber's hosts".to_owned(),
            ));
        }
        Ok(AdminUser(user))
    }
}
