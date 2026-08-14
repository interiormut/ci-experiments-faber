//! One SearXNG instance.
//!
//! The whole protocol lives here: [`crate::public`] composes these clients and
//! adds scheduling, it does not reimplement the wire. Bare SearXNG is a strict
//! subset of the public-network provider, and keeping it that way is what
//! stops the two from drifting.
//!
//! ## Requirements on the instance
//!
//! The JSON API must be enabled — `settings.yml` needs `search.formats`
//! to include `json`, and the stock configuration does not. An instance
//! without it answers a `format=json` request with the ordinary HTML search
//! page under a `200`, which this crate reports as
//! [`Error::NoJsonApi`] rather than pretending a successful empty result. If
//! the instance also runs the default limiter, a non-browser client gets
//! `429` regardless of format, and its `limiter.toml` needs to let this client
//! through.

mod wire;

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode};
use url::Url;

use crate::endpoint::endpoint;
use crate::engine::SearchEngine;
use crate::error::{Error, Result};
use crate::types::{Query, Results};

/// Sent unless [`Config::with_user_agent`] says otherwise.
///
/// Honest about who is calling. Public instances are entitled to know, and
/// impersonating a browser to slip past a limiter is asking an operator for
/// resources under false pretences.
pub const DEFAULT_USER_AGENT: &str = concat!(
    "faber-search/",
    env!("CARGO_PKG_VERSION"),
    " (+SearXNG JSON API client)"
);

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// How to reach one instance.
#[derive(Clone, Debug)]
pub struct Config {
    base_url: Url,
    user_agent: String,
    timeout: Duration,
    proxy: Option<String>,
}

impl Config {
    /// `base_url` is the instance root — `https://searx.example.org`, or
    /// `https://example.org/searx` when it is published under a path. Any path
    /// given is preserved; `/search` is appended to it.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let base_url = Url::parse(base_url.as_ref())
            .map_err(|error| Error::Config(format!("base URL: {error}")))?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(Error::Config(format!(
                "base URL must be http or https, got `{}`",
                base_url.scheme()
            )));
        }
        Ok(Config {
            base_url,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            proxy: None,
        })
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Routes through an explicit proxy.
    ///
    /// The only way to get one. Faber runs many users' work in one process, so
    /// `reqwest`'s default of reading `HTTP_PROXY`/`HTTPS_PROXY` from the
    /// process environment would put the host's network configuration into
    /// every user's search path; [`Config::client`] turns that off.
    pub fn with_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    /// Builds the HTTP client this config describes.
    ///
    /// Public so a pool can build **one** client and share it across every
    /// instance it talks to: connection pooling and DNS caching are per-client,
    /// and a client per instance would throw both away.
    pub fn client(&self) -> Result<Client> {
        crate::http::client(&self.user_agent, self.timeout, self.proxy.as_deref())
    }
}

/// A client for one instance.
#[derive(Clone, Debug)]
pub struct SearxNg {
    http: Client,
    base_url: Url,
    /// The host, for [`SearchEngine::provider`] and for pool logs.
    name: String,
}

impl SearxNg {
    pub fn new(config: Config) -> Result<Self> {
        let http = config.client()?;
        Ok(SearxNg::with_client(http, config.base_url))
    }

    /// Shares an existing client. What [`crate::public`] uses.
    pub fn with_client(http: Client, base_url: Url) -> Self {
        let name = base_url.host_str().unwrap_or("searxng").to_owned();
        SearxNg {
            http,
            base_url,
            name,
        }
    }

    /// The instance root this client was built for.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    fn request_url(&self, query: &Query) -> Result<Url> {
        let mut url = endpoint(self.base_url.clone(), ["search"])?;
        {
            let mut params = url.query_pairs_mut();
            params.append_pair("q", &query.text);
            params.append_pair("format", "json");
            if query.page > 1 {
                params.append_pair("pageno", &query.page.to_string());
            }
            if !query.categories.is_empty() {
                params.append_pair("categories", &query.categories.join(","));
            }
            if !query.engines.is_empty() {
                params.append_pair("engines", &query.engines.join(","));
            }
            if let Some(language) = &query.language {
                params.append_pair("language", language);
            }
            if let Some(range) = query.time_range {
                params.append_pair("time_range", range.as_param());
            }
            params.append_pair("safesearch", query.safe_search.as_param());
        }
        Ok(url)
    }

    async fn run(&self, query: &Query) -> Result<Results> {
        let url = self.request_url(query)?;
        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;

        let response = check_status(response).await?;
        let body = read_json(response).await?;
        let mut results = body.into_results(query.limit);
        results.source = Some(self.name.clone());
        Ok(results)
    }
}

#[async_trait]
impl SearchEngine for SearxNg {
    async fn search(&self, query: &Query) -> Result<Results> {
        self.run(query).await
    }

    fn provider(&self) -> &str {
        &self.name
    }
}

/// Turns a non-success status into the error that says what to do about it.
async fn check_status(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(Error::RateLimited {
            retry_after: retry_after(&response),
        });
    }

    // A `403` is ambiguous and stays that way: it is `search.formats` refusing
    // the format on some instances and the bot limiter refusing *us* on
    // others, and nothing in the response distinguishes them. Reporting it as
    // [`Error::NoJsonApi`] would be a permanent verdict on a coin flip, so it
    // is left as a plain status that [`Error::is_throttle`] classifies as
    // "later, elsewhere".
    let message = response
        .text()
        .await
        .ok()
        .map(|body| summarise(&body))
        .unwrap_or_else(|| status.to_string());
    Err(Error::Api {
        status: status.as_u16(),
        message,
    })
}

/// Reads a success response, insisting it really is JSON.
///
/// The interesting case is a `200` carrying the HTML search page: an instance
/// without `json` in `search.formats` ignores the parameter and renders the
/// page. Decoding that as "no results" would quietly report an empty web, so
/// it is a typed refusal that also tells a pool to give up on this instance.
async fn read_json(response: Response) -> Result<wire::Response> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let looks_like_json = content_type.contains("json");

    let body = response.text().await?;
    if !looks_like_json {
        return Err(Error::NoJsonApi {
            reason: format!(
                "instance answered a format=json request with `{}`",
                if content_type.is_empty() {
                    "no content type"
                } else {
                    content_type.split(';').next().unwrap_or(&content_type)
                }
            ),
        });
    }
    serde_json::from_str(&body)
        .map_err(|error| Error::Decode(format!("{error}: {}", summarise(&body))))
}

fn retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Error bodies are frequently a whole HTML page; a log line is not.
fn summarise(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 200 {
        return trimmed.to_owned();
    }
    // Byte 200 may land inside a character; walk back to a boundary.
    let mut end = 200;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SafeSearch, TimeRange};

    fn client() -> SearxNg {
        SearxNg::new(Config::new("https://searx.example.org").unwrap()).unwrap()
    }

    #[test]
    fn minimal_query_sends_only_what_it_must() {
        let url = client().request_url(&Query::new("rust ownership")).unwrap();
        assert_eq!(url.path(), "/search");
        let params: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            params,
            [
                ("q".into(), "rust ownership".to_string()),
                ("format".into(), "json".into()),
                ("safesearch".into(), "0".into()),
            ]
        );
    }

    #[test]
    fn full_query_maps_every_field() {
        let query = Query::new("weather")
            .with_page(3)
            .with_categories(["news", "general"])
            .with_engines(["duckduckgo"])
            .with_language("en-US")
            .with_time_range(TimeRange::Week)
            .with_safe_search(SafeSearch::Strict);
        let url = client().request_url(&query).unwrap();
        let params: std::collections::HashMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(params["pageno"], "3");
        assert_eq!(params["categories"], "news,general");
        assert_eq!(params["engines"], "duckduckgo");
        assert_eq!(params["language"], "en-US");
        assert_eq!(params["time_range"], "week");
        assert_eq!(params["safesearch"], "2");
    }

    #[test]
    fn a_base_path_survives() {
        let engine = SearxNg::new(Config::new("https://example.org/searx/").unwrap()).unwrap();
        let url = engine.request_url(&Query::new("x")).unwrap();
        assert_eq!(url.path(), "/searx/search");
    }

    #[test]
    fn rejects_a_non_http_base() {
        assert!(matches!(
            Config::new("ftp://example.org"),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn summarise_does_not_split_a_character() {
        let body = "가".repeat(300);
        let short = summarise(&body);
        assert!(short.len() <= 203 && short.ends_with('…'));
    }
}
