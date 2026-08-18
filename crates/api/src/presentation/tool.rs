//! API-owned model projection for live HTTP presentations.
//!
//! The harness only routes this tool.  URI policy, the session context, and
//! lifecycle persistence stay on the API side of that boundary.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use uuid::Uuid;

use harness::{mapping::ToolResult, state::ToolInvoker};

use crate::{
    presentation::{Options, Target, present},
    state::AppState,
};

const TITLE_LIMIT: usize = 200;

/// The normalized part of a model request that the currently available live
/// backend needs.  Keep parsing here rather than teaching the proxy about
/// model input or URL spellings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub environment_label: String,
    pub port: u16,
    pub render: Render,
    pub title: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Render {
    #[default]
    Auto,
    Webpage,
    Document,
    Source,
    Shader,
    Media,
}

impl Render {
    fn parse(value: Option<&Value>) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self::Auto);
        };
        let object = value
            .as_object()
            .ok_or_else(|| "`render` must be an object".to_owned())?;
        if object.len() != 1 || !object.contains_key("type") {
            return Err("`render` only accepts the required `type` property".to_owned());
        }
        match object.get("type").and_then(Value::as_str) {
            Some("auto") => Ok(Self::Auto),
            Some("webpage") => Ok(Self::Webpage),
            Some("document") => Ok(Self::Document),
            Some("source") => Ok(Self::Source),
            Some("shader") => Ok(Self::Shader),
            Some("media") => Ok(Self::Media),
            Some(_) => Err(
                "`render.type` must be auto, webpage, document, source, shader, or media"
                    .to_owned(),
            ),
            None => Err("`render.type` is required and must be a string".to_owned()),
        }
    }
}

/// The API projection installed into one run's toolbox.
pub struct PresentTool {
    state: AppState,
    actor: Uuid,
    session_id: Uuid,
}

impl PresentTool {
    pub fn new(state: AppState, actor: Uuid, session_id: Uuid) -> Self {
        Self {
            state,
            actor,
            session_id,
        }
    }

    pub fn definitions() -> Vec<llm::ToolDef> {
        vec![llm::ToolDef {
            name: "present".to_owned(),
            description: "Present exactly one user-accessible target in the side panel and add an openable block to the chat.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": { "type": "string", "description": "The bound environment to execute in. Call `bound_environments` to list them." },
                    "target": { "type": "string", "format": "uri", "pattern": "^(?:file|http)://", "description": "Target URI. Supported schemes: file and http." },
                    "render": { "type": "object", "properties": { "type": { "type": "string", "enum": ["auto", "webpage", "document", "source", "shader", "media"], "default": "auto" } }, "required": ["type"], "additionalProperties": false },
                    "snapshot": { "type": "boolean", "default": false, "description": "For file targets, whether to capture immutable contents now. Must be false for http targets." },
                    "title": { "type": "string", "description": "Optional display title for the presentation." }
                },
                "required": ["execute_in", "target"],
                "additionalProperties": false
            }),
        }]
    }

    pub fn invoker(self: Arc<Self>) -> ToolInvoker {
        Arc::new(move |name, input| {
            let tool = Arc::clone(&self);
            Box::pin(async move {
                if name != "present" {
                    return Err(format!("`{name}` is not a presentation tool"));
                }
                match tool.invoke(input).await {
                    Ok(content) => Ok(ToolResult {
                        content,
                        is_error: false,
                    }),
                    Err(content) => Ok(ToolResult {
                        content,
                        is_error: true,
                    }),
                }
            }) as BoxFuture<'static, Result<ToolResult, String>>
        })
    }

    async fn invoke(&self, input: Value) -> Result<String, String> {
        let request = parse_http_request(input)?;
        let presented = present(
            &self.state,
            self.actor,
            &Target {
                session_id: self.session_id,
                environment_label: request.environment_label,
                port: request.port,
            },
            Options::default(),
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(format!("Presentation ready: {}", presented.url))
    }
}

pub fn parse_http_request(input: Value) -> Result<HttpRequest, String> {
    let object = input
        .as_object()
        .ok_or_else(|| "`present` input must be an object".to_owned())?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "execute_in" | "target" | "render" | "snapshot" | "title"
        ) {
            return Err(format!("`{key}` is not accepted by `present`"));
        }
    }
    let environment_label = object
        .get("execute_in")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "`execute_in` is required and must be a non-empty string".to_owned())?
        .to_owned();
    let target = object
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| "`target` is required and must be a string".to_owned())?;
    let snapshot = object
        .get("snapshot")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "`snapshot` must be a boolean".to_owned())
        })
        .transpose()?
        .unwrap_or(false);
    let url = url::Url::parse(target).map_err(|_| "`target` must be an absolute URI".to_owned())?;
    if url.scheme() == "file" {
        return Err(
            "file presentations are not available until the raw-byte file backend is configured"
                .to_owned(),
        );
    }
    if url.scheme() != "http" {
        return Err("`target` must use the file or http scheme; https is not supported".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("HTTP presentation targets cannot contain user credentials".to_owned());
    }
    if snapshot {
        return Err("`snapshot:true` is only valid for file targets".to_owned());
    }
    if url.fragment().is_some() {
        return Err("HTTP presentation targets cannot contain a fragment".to_owned());
    }
    if url.query().is_some() {
        return Err("HTTP presentation targets cannot contain a query".to_owned());
    }
    if url.path() != "/" {
        return Err("HTTP presentation targets must use the service origin (path `/`)".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "HTTP presentation target requires a host".to_owned())?;
    if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err(
            "HTTP presentation target host must be localhost, 127.0.0.1, or [::1]".to_owned(),
        );
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "HTTP presentation target requires a valid port".to_owned())?;
    if port == 0 {
        return Err("HTTP presentation target port must be non-zero".to_owned());
    }
    let title = match object.get("title") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.chars().count() <= TITLE_LIMIT => Some(value.clone()),
        Some(Value::String(_)) => {
            return Err(format!("`title` must be {TITLE_LIMIT} characters or fewer"));
        }
        Some(_) => return Err("`title` must be a string".to_owned()),
    };
    Ok(HttpRequest {
        environment_label,
        port,
        render: Render::parse(object.get("render"))?,
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_loopback_http_origins_are_accepted() {
        let input = |target| json!({"execute_in":"dev", "target":target});
        assert_eq!(
            parse_http_request(input("http://localhost:5173"))
                .unwrap()
                .port,
            5173
        );
        assert!(parse_http_request(input("https://localhost:5173")).is_err());
        assert!(parse_http_request(input("http://example.com:5173")).is_err());
        assert!(parse_http_request(input("http://user@localhost:5173")).is_err());
        assert!(parse_http_request(input("http://localhost:5173/app")).is_err());
        assert!(
            parse_http_request(
                json!({"execute_in":"dev", "target":"http://localhost:5173", "snapshot":true})
            )
            .is_err()
        );
    }
}
