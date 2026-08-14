//! Building the one search engine this service runs on.
//!
//! Built once, at boot, and held in [`AppState`](crate::state::AppState) —
//! never per run. The public provider discovers `searx.space` and then probes
//! instances for the JSON API, which is a directory fetch plus a burst of
//! requests; doing that on the run path would put it on every user's first
//! turn and lean on other people's instances once per run rather than once per
//! process.
//!
//! Absent is a supported answer. A service with no engine configured grants no
//! `search` tool, and a run there simply has one move fewer.

use std::sync::Arc;
use std::time::Duration;

use search::{SearchEngine, parallel, public, searxng};

use crate::config::Config;

/// How long boot will wait for discovery before giving up on it.
///
/// Bounded here rather than left to the provider's own timeouts: those bound a
/// request, and discovery is many. A service that cannot start because
/// `searx.space` is slow is a worse failure than a service that starts without
/// search.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(45);

/// Builds the engine the operator configured, or none.
///
/// A named instance wins over the public network: it is the more specific
/// instruction, and an operator who runs their own instance is not asking to
/// be load-balanced across strangers'.
pub async fn build_engine(config: &Config) -> Option<Arc<dyn SearchEngine>> {
    if let Some(url) = &config.searxng_url {
        let mut settings = match searxng::Config::new(url) {
            Ok(settings) => settings,
            Err(error) => {
                // Loud, and not fatal: a mistyped search URL should not keep
                // the service from serving everything that is not search.
                tracing::error!(%url, %error, "SEARXNG_URL is not usable; search is disabled");
                return None;
            }
        };
        if let Some(proxy) = &config.search_proxy {
            settings = settings.with_proxy(proxy.clone());
        }
        return match searxng::SearxNg::new(settings) {
            Ok(engine) => {
                tracing::info!(%url, "search: one named SearXNG instance");
                Some(Arc::new(engine))
            }
            Err(error) => {
                tracing::error!(%url, %error, "could not build a search client; search is disabled");
                None
            }
        };
    }

    if let Some(api_key) = &config.parallel_api_key {
        let mut settings = parallel::Config::new(api_key);
        if let Some(proxy) = &config.search_proxy {
            settings = settings.with_proxy(proxy.clone());
        }
        return match parallel::Parallel::new(settings) {
            Ok(engine) => {
                tracing::info!("search: Parallel");
                Some(Arc::new(engine))
            }
            Err(error) => {
                tracing::error!(%error, "could not build Parallel search client; search is disabled");
                None
            }
        };
    }

    if !config.search_public_network {
        tracing::info!("search: not configured, so no run is granted the tool");
        return None;
    }

    let mut settings = public::Config::default();
    if let Some(proxy) = &config.search_proxy {
        settings = settings.with_proxy(proxy.clone());
    }

    // Boot waits for this. Discovery is the price of the public network and it
    // is paid once; a service that started serving first would hand its
    // earliest runs an engine with nothing verified behind it.
    match tokio::time::timeout(DISCOVERY_BUDGET, public::PublicSearxNg::discover(settings)).await {
        Ok(Ok(engine)) => {
            let census = engine.census();
            tracing::info!(?census, "search: the public SearXNG network");
            Some(Arc::new(engine))
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "the public search network could not be discovered; search is disabled");
            None
        }
        Err(_) => {
            tracing::error!(
                budget = ?DISCOVERY_BUDGET,
                "discovering the public search network took too long; search is disabled"
            );
            None
        }
    }
}
