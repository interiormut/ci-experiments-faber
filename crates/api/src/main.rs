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

    let auth = build_auth_provider(&config).await;

    // The browser-facing perimeter, mounted at `/v1` on faber's own origin. In remote
    // mode this reverse-proxies to surge-server, so the frontend only ever talks to
    // faber and never learns whether Surge is embedded or served.
    //
    // The embedded-only fields are ignored by `RemoteProvider` — upstream owns rate
    // limiting, registration policy, and the maintenance sweep.
    let browser_router = Arc::clone(&auth).browser_router(surge::router::BrowserRouterConfig {
        cookie_domain: config.surge_cookie_domain.clone(),
        session_ttl: config.surge_session_ttl,
        auth_ui_origin: config.surge_auth_ui_origin.clone(),
        session_cors_origins: vec![],
        rate_limiter: None,
        return_origins: None,
        registration: None,
        factor_policy: None,
        allow_inline: None,
        oauth_bridge: None,
        maintenance_interval: None,
    });

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

    // `cors` covers faber's own routes only — the browser router brings its own policy,
    // scoped to the credential-entry and session-management zones.
    let app = routes::router()
        .layer(cors)
        .with_state(state)
        .merge(browser_router)
        .layer(TraceLayer::new_for_http());

    let addr = format!("0.0.0.0:{}", config.api_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    tracing::info!("listening on {addr}");
    // `into_make_service_with_connect_info` is what makes the peer address visible to
    // the proxy, which forwards it upstream so rate limits key on the end user.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}

/// Builds the remote provider that talks to `surge-server`.
async fn remote_auth_provider(config: &config::Config) -> Arc<dyn surge::AuthProvider> {
    let service_token = config
        .surge_service_token
        .clone()
        .expect("SURGE_SERVICE_TOKEN must be set");

    surge::remote(surge::RemoteConfig {
        base_url: config.surge_url.parse().expect("invalid SURGE_URL"),
        service_token: secrecy::SecretString::from(service_token),
        cache_ttl: std::time::Duration::from_secs(30),
        cache_max_entries: 10_000,
        timeout: std::time::Duration::from_secs(3),
    })
    .await
    .expect("failed to build surge auth provider")
}

/// Selects the auth provider. `TestProvider` needs two locks turned: the
/// `test-provider` Cargo feature at build time and `SURGE_TEST_PROVIDER=true` at
/// run time. It authenticates *every* request as a fixed identity, so a
/// production binary must be built without the feature — the env var alone can
/// then do nothing.
#[cfg(feature = "test-provider")]
async fn build_auth_provider(config: &config::Config) -> Arc<dyn surge::AuthProvider> {
    if std::env::var("SURGE_TEST_PROVIDER").as_deref() == Ok("true") {
        let username =
            std::env::var("SURGE_TEST_USERNAME").unwrap_or_else(|_| "test-user".to_owned());
        let display_name =
            std::env::var("SURGE_TEST_DISPLAY_NAME").unwrap_or_else(|_| "Test User".to_owned());

        return surge::test(surge::TestConfig {
            username,
            display_name,
        })
        .expect("failed to build test auth provider");
    }

    remote_auth_provider(config).await
}

#[cfg(not(feature = "test-provider"))]
async fn build_auth_provider(config: &config::Config) -> Arc<dyn surge::AuthProvider> {
    remote_auth_provider(config).await
}
