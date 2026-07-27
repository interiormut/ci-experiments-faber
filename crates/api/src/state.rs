use axum::extract::FromRef;
use std::sync::Arc;

use crate::{config::Config, db::DbPool};

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub config: Config,
    pub http: reqwest::Client,
    /// Surge session/identity provider (remote — talks to `surge-server` over HTTP).
    pub auth: Arc<dyn surge::AuthProvider>,
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
