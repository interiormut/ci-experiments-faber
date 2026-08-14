//! Parallel's hosted web search API.

mod wire;

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode};

use crate::engine::SearchEngine;
use crate::error::{Error, Result};
use crate::types::{Hit, Query, Results};

const API_URL: &str = "https://api.parallel.ai/v1/search";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Configuration for the Parallel Search API.
#[derive(Clone)]
pub struct Config {
    api_key: String,
    timeout: Duration,
    proxy: Option<String>,
}

impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        Config {
            api_key: api_key.into(),
            timeout: DEFAULT_TIMEOUT,
            proxy: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Routes Parallel traffic through an explicit proxy.
    pub fn with_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    fn client(&self) -> Result<Client> {
        crate::http::client(
            concat!(
                "faber-search/",
                env!("CARGO_PKG_VERSION"),
                " (+Parallel API client)"
            ),
            self.timeout,
            self.proxy.as_deref(),
        )
    }
}

/// A client for Parallel's Search API.
#[derive(Clone)]
pub struct Parallel {
    http: Client,
    api_key: String,
}

impl Parallel {
    pub fn new(config: Config) -> Result<Self> {
        if config.api_key.trim().is_empty() {
            return Err(Error::Config("Parallel API key is empty".to_owned()));
        }
        Ok(Parallel {
            http: config.client()?,
            api_key: config.api_key,
        })
    }

    async fn run(&self, query: &Query) -> Result<Results> {
        let request = wire::Request {
            objective: &query.text,
            search_queries: vec![query.text.clone()],
            advanced_settings: query
                .limit
                .map(|limit| wire::AdvancedSettings { max_results: limit }),
        };
        let response = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_else(|_| status.to_string());
            return Err(if status == StatusCode::TOO_MANY_REQUESTS {
                Error::RateLimited { retry_after: None }
            } else {
                Error::Api {
                    status: status.as_u16(),
                    message,
                }
            });
        }

        let body: wire::Response = response
            .json()
            .await
            .map_err(|error| Error::Decode(error.to_string()))?;
        let mut hits: Vec<Hit> = body
            .results
            .into_iter()
            .filter_map(wire::ResultItem::into_hit)
            .collect();
        if let Some(limit) = query.limit {
            hits.truncate(limit);
        }
        Ok(Results {
            query: query.text.clone(),
            hits,
            source: Some("parallel".to_owned()),
            ..Results::default()
        })
    }
}

#[async_trait]
impl SearchEngine for Parallel {
    async fn search(&self, query: &Query) -> Result<Results> {
        self.run(query).await
    }

    fn provider(&self) -> &str {
        "parallel"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_empty_api_key() {
        assert!(matches!(
            Parallel::new(Config::new(" ")),
            Err(Error::Config(_))
        ));
    }
}
