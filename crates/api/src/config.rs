use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub surge_url: String,
    pub surge_service_token: String,
    pub surge_cookie_domain: String,
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
