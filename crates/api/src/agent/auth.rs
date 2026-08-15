//! Machine auth for the agent-transport connect endpoint (X41).
//!
//! Deliberately not `SurgeIdentity`/`AuthUser`: those verify a human's
//! session, and an agent connection arrives unsolicited from a daemon Faber
//! did not dial, running on infrastructure Faber does not control. What is
//! being checked is "did we issue this token", not "which user is this" —
//! R15's argument for why this needs its own extractor rather than a bent
//! version of theirs.

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
};
use base64::Engine as _;
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use sha2::{Digest, Sha256};

use crate::{error::AppError, models::agent::AgentCredential, schema::agent_credential, state::AppState};

/// Turns a bearer token into the value stored in `token_hash`. SHA-256
/// rather than a slow KDF, deliberately: this hashes a 256-bit CSPRNG value,
/// not a human-chosen password, so there is no low-entropy guessing surface
/// for a slow hash to defend against — only a lookup key to compute.
pub fn hash_token(token: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()))
}

/// A daemon's connection credential, verified against `agent_credential`.
pub struct AgentIdentity {
    pub credential: AgentCredential,
}

impl FromRequestParts<AppState> for AgentIdentity {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| AppError::Unauthorized("missing agent credential".into()))?;

        let hash = hash_token(token);
        let mut conn = state.db.get().await?;
        let credential: Option<AgentCredential> = agent_credential::table
            .filter(agent_credential::token_hash.eq(&hash))
            .filter(agent_credential::revoked_at.is_null())
            .select(AgentCredential::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|err| AppError::db(err, "agent.auth.lookup"))?;

        let credential =
            credential.ok_or_else(|| AppError::Unauthorized("unknown agent credential".into()))?;

        Ok(AgentIdentity { credential })
    }
}
