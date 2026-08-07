//! The OpenAI Chat Completions API.
//!
//! Rust has no first-party OpenAI SDK in this workspace, so this speaks the
//! REST API over `reqwest` directly: `POST /v1/chat/completions` with
//! `stream: true`, framed as SSE.
//!
//! The client holds a base URL, an HTTP client, and a credential. It reads no
//! environment variables and owns no global state — everything it can do
//! arrives through [`Config`], so a workflow that withholds one cannot be
//! bypassed.

mod wire;

use futures_util::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::time::Duration;
use url::Url;

use crate::client::{EventStream, ModelClient, endpoint};
use crate::error::{Error, Result};
use crate::span::RenderedRequest;
use crate::sse;
use crate::types::Request;

/// Default model id. Nothing in this crate reads it implicitly — it exists so
/// a caller with no opinion has one to name.
pub const DEFAULT_MODEL: &str = "gpt-5";

/// Everything the client is allowed to do.
pub struct Config {
    pub api_key: SecretString,
    /// Defaults to `https://api.openai.com`.
    pub base_url: Option<Url>,
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
            http: None,
        }
    }
}

/// A handle to the OpenAI Chat Completions API.
pub struct OpenAI {
    http: reqwest::Client,
    endpoint: Url,
    api_key: SecretString,
}

impl OpenAI {
    pub fn new(config: Config) -> Result<Self> {
        let base = match config.base_url {
            Some(url) => url,
            None => Url::parse("https://api.openai.com").expect("valid literal URL"),
        };
        let endpoint = endpoint(base, ["v1", "chat", "completions"])?;

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
        })
    }

    fn build(&self, body: Vec<u8>) -> reqwest::RequestBuilder {
        self.http
            .post(self.endpoint.clone())
            .header(
                "authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body)
    }
}

impl ModelClient for OpenAI {
    fn provider(&self) -> &str {
        "openai"
    }

    fn render(&self, request: &Request) -> Result<RenderedRequest> {
        wire::render(request)
    }

    fn send(&self, rendered: RenderedRequest) -> EventStream<'_> {
        let model = rendered.prefix.model.clone();
        let builder = self.build(rendered.body);

        Box::pin(async_stream::try_stream! {
            let response = builder.send().await?;
            let status = response.status();
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                Err(api_error(status.as_u16(), request_id, &body))?;
                return;
            }

            tracing::debug!(model = %model, "openai stream opened");

            let mut frames = Box::pin(sse::frames(response.bytes_stream()));
            let mut decoder = wire::StreamDecoder::default();
            while let Some(frame) = frames.next().await {
                let frame: Value = frame?;
                for event in decoder.push_chunk(&frame)? {
                    yield event;
                }
            }
            for event in decoder.finish() {
                yield event;
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
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert!(error.is_transient());
        assert!(error.to_string().contains("slow down"));
    }

    #[test]
    fn non_json_error_body_survives() {
        let error = api_error(502, None, "<html>bad gateway</html>");
        assert!(error.to_string().contains("bad gateway"));
    }

    fn endpoint_for(base: &str) -> String {
        let mut config = Config::new("k".into());
        config.base_url = Some(Url::parse(base).unwrap());
        OpenAI::new(config).unwrap().endpoint.to_string()
    }

    #[test]
    fn the_default_endpoint_is_the_public_api() {
        let client = OpenAI::new(Config::new("k".into())).unwrap();
        assert_eq!(
            client.endpoint.as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn a_gateway_path_prefix_is_kept() {
        // The case `base_url` exists for: dropping the prefix would route the
        // request past the gateway entirely.
        for base in [
            "https://gw.example.com/openai",
            "https://gw.example.com/openai/",
        ] {
            assert_eq!(
                endpoint_for(base),
                "https://gw.example.com/openai/v1/chat/completions"
            );
        }
    }
}
