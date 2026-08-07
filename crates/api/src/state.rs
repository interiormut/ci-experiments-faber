use axum::extract::FromRef;
use std::sync::Arc;

use crate::{config::Config, db::DbPool};

/// 32-byte master key for envelope encryption. Never printed — Debug is intentionally redacted.
#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub config: Config,
    pub http: reqwest::Client,
    /// Surge session/identity provider (remote — talks to `surge-server` over HTTP).
    pub auth: Arc<dyn surge::AuthProvider>,
    /// Envelope encryption master key. Loaded at boot; absent key panics before serving traffic.
    pub master_key: Arc<MasterKey>,
}

impl FromRef<AppState> for DbPool {
    fn from_ref(s: &AppState) -> Self {
        s.db.clone()
    }
}

impl FromRef<AppState> for Config {
    fn from_ref(s: &AppState) -> Self {
        s.config.clone()
    }
}
