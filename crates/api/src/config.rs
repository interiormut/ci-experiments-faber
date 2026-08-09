use std::env;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub surge_url: String,
    pub surge_service_token: String,
    pub surge_cookie_domain: String,
    /// Origin serving the auth UI. The browser perimeter redirects here to start a
    /// login flow, and it is the sole allowed origin for credential entry.
    pub surge_auth_ui_origin: String,
    /// Session lifetime advertised by the proxied perimeter. Matches upstream's.
    pub surge_session_ttl: Duration,
    pub api_port: u16,
    pub cors_origins: Vec<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            surge_url: env::var("SURGE_URL").unwrap_or_else(|_| "http://localhost:3000".to_owned()),
            surge_service_token: env::var("SURGE_SERVICE_TOKEN")
                .expect("SURGE_SERVICE_TOKEN must be set"),
            surge_cookie_domain: env::var("SURGE_COOKIE_DOMAIN")
                .unwrap_or_else(|_| ".panit.dev".to_owned()),
            surge_auth_ui_origin: env::var("SURGE_AUTH_UI_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:3000".to_owned()),
            surge_session_ttl: env::var("SURGE_SESSION_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(72 * 3600)),
            api_port: env::var("API_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3001),
            cors_origins: env::var("CORS_ORIGIN")
                .ok()
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        }
    }
}
