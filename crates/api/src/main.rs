use base64::Engine as _;
use diesel::Connection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod config;
mod crypto;
mod db;
mod error;
mod models;
mod resolve;
mod routes;
mod schema;
mod state;

use state::{AppState, MasterKey};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "api=debug,tower_http=debug".parse().unwrap()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::Config::from_env();

    let master_key = {
        let raw = std::env::var("FABER_MASTER_KEY")
            .expect("FABER_MASTER_KEY must be set (32 random bytes, base64-encoded)");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .expect("FABER_MASTER_KEY must be valid base64");
        let arr: [u8; 32] = decoded
            .try_into()
            .expect("FABER_MASTER_KEY must decode to exactly 32 bytes");
        Arc::new(MasterKey::new(arr))
    };

    {
        let mut sync_conn = diesel::PgConnection::establish(&config.database_url)
            .expect("failed to connect to Postgres for migrations");
        sync_conn
            .run_pending_migrations(MIGRATIONS)
            .expect("failed to run database migrations");
        tracing::info!("migrations applied");
    }

    let db = db::init_pool(&config.database_url).await;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    let auth = surge::remote(surge::RemoteConfig {
        base_url: config.surge_url.parse().expect("invalid SURGE_URL"),
        service_token: secrecy::SecretString::from(config.surge_service_token.clone()),
        cache_ttl: std::time::Duration::from_secs(30),
        cache_max_entries: 10_000,
        timeout: std::time::Duration::from_secs(3),
    })
    .await
    .expect("failed to build surge auth provider");

    let state = AppState {
        db,
        config: config.clone(),
        http,
        auth,
        master_key,
    };

    let cors_origins = config
        .cors_origins
        .iter()
        .map(|origin| origin.parse().expect("invalid CORS origin"))
        .collect::<Vec<_>>();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(cors_origins))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .allow_credentials(true);

    let app = routes::router()
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.api_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    tracing::info!("listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}
