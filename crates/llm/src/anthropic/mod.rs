//! The Anthropic Messages API.
//!
//! Rust has no first-party Anthropic SDK, so this speaks the REST API over
//! `reqwest` directly: `POST /v1/messages` with `stream: true`, framed as SSE.
//!
//! The client holds a base URL, an HTTP client, and a credential. It reads no
//! environment variables and owns no global state — everything it can do
//! arrives through [`Config`], so a workflow that withholds one cannot be
//! bypassed.

mod sse;
mod wire;

use futures_util::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::time::Duration;
use url::Url;

use crate::client::{EventStream, ModelClient};
use crate::error::Error;
use crate::types::Request;

/// The API version header value this client is written against.
pub const API_VERSION: &str = "2023-06-01";

/// Default model id. Nothing in this crate reads it implicitly — it exists so
/// a caller with no opinion has one to name.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// Everything the client is allowed to do.
pub struct Config {
    pub api_key: SecretString,
    /// Defaults to `https://api.anthropic.com`.
    pub base_url: Option<Url>,
    /// `anthropic-beta` opt-ins, sent on every request.
    pub betas: Vec<String>,
    /// An existing HTTP client to share connection pooling with.
    ///
    /// If you supply one, give it a connect timeout and a read timeout but
    /// **not** a total request timeout: a long generation legitimately holds
    /// the connection open for minutes, and a total timeout would cut it off
    /// mid-answer. If absent, one is built with [`CONNECT_TIMEOUT`] and
    /// [`READ_TIMEOUT`] — `reqwest`'s own default has neither, so a stalled
    /// server would hang the call forever.
    pub http: Option<reqwest::Client>,
}

/// How long to wait for the connection itself.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait between bytes once the response is streaming. Bounds a
/// stalled connection without bounding the generation.
pub const READ_TIMEOUT: Duration = Duration::from_secs(120);

impl Config {
    pub fn new(api_key: SecretString) -> Self {
        Self {
            api_key,
            base_url: None,
            betas: Vec::new(),
            http: None,
        }
    }
}

/// A handle to the Anthropic Messages API.
pub struct Anthropic {
    http: reqwest::Client,
    endpoint: Url,
    api_key: SecretString,
    betas: Option<String>,
}

impl Anthropic {
    pub fn new(config: Config) -> Result<Self, Error> {
        let base = match config.base_url {
            Some(url) => url,
            None => Url::parse("https://api.anthropic.com").expect("valid literal URL"),
        };
        let endpoint = base
            .join("/v1/messages")
            .map_err(|source| Error::Decode(format!("invalid base URL: {source}")))?;

        let http = match config.http {
            Some(http) => http,
            None => reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                .build()?,
        };

        Ok(Self {
            http,
            endpoint,
            api_key: config.api_key,
            betas: (!config.betas.is_empty()).then(|| config.betas.join(",")),
        })
    }

    fn build(&self, request: &Request) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(self.endpoint.clone())
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&wire::request_body(request));

        if let Some(betas) = &self.betas {
            builder = builder.header("anthropic-beta", betas);
        }
        builder
    }
}

impl ModelClient for Anthropic {
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn stream(&self, request: Request) -> EventStream<'_> {
        let builder = self.build(&request);
        let model = request.model.clone();

        Box::pin(async_stream::try_stream! {
            let response = builder.send().await?;
            let status = response.status();
            let request_id = response
                .headers()
                .get("request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                Err(api_error(status.as_u16(), request_id, &body))?;
                return;
            }

            tracing::debug!(model = %model, "anthropic stream opened");

            let mut frames = Box::pin(sse::frames(response.bytes_stream()));
            while let Some(frame) = frames.next().await {
                let frame: Value = frame?;
                if let Some(event) = wire::parse_event(&frame)? {
                    yield event;
                }
            }
        })
    }
}

/// Unpacks the provider's error envelope, falling back to the raw body when
/// it isn't shaped the way we expect.
fn api_error(status: u16, request_id: Option<String>, body: &str) -> Error {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().and_then(|value| value.get("error"));

    Error::Api {
        status: Some(status),
        kind: error
            .and_then(|error| error.get("type"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        message: error
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| body.trim().to_owned()),
        request_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelope_is_unpacked() {
        let error = api_error(
            429,
            Some("req_1".into()),
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert!(error.is_transient());
        assert!(error.to_string().contains("slow down"));
    }

    #[test]
    fn non_json_error_body_survives() {
        let error = api_error(502, None, "<html>bad gateway</html>");
        assert!(error.to_string().contains("bad gateway"));
    }
}
