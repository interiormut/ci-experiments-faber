use serde::{Deserialize, Serialize};

use crate::types::Hit;

#[derive(Debug, Serialize)]
pub(super) struct Request<'a> {
    pub objective: &'a str,
    pub search_queries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advanced_settings: Option<AdvancedSettings>,
}

#[derive(Debug, Serialize)]
pub(super) struct AdvancedSettings {
    pub max_results: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct Response {
    #[serde(default)]
    pub results: Vec<ResultItem>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ResultItem {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub publish_date: Option<String>,
    #[serde(default)]
    pub excerpts: Vec<String>,
}

impl ResultItem {
    pub(super) fn into_hit(self) -> Option<Hit> {
        if self.url.is_empty() {
            return None;
        }
        Some(Hit {
            url: self.url,
            title: self.title.unwrap_or_default(),
            snippet: self.excerpts.join("\n\n"),
            engines: vec!["parallel".to_owned()],
            score: 0.0,
            category: None,
            published: self.publish_date.filter(|date| !date.is_empty()),
            thumbnail: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_parallel_results() {
        let response: Response = serde_json::from_str(
            r#"{
                "search_id": "search_123",
                "results": [{
                    "url": "https://example.org",
                    "title": "Example",
                    "publish_date": "2025-01-02",
                    "excerpts": ["First excerpt", "Second excerpt"]
                }]
            }"#,
        )
        .unwrap();
        let hit = response
            .results
            .into_iter()
            .next()
            .unwrap()
            .into_hit()
            .unwrap();
        assert_eq!(hit.url, "https://example.org");
        assert_eq!(hit.title, "Example");
        assert_eq!(hit.snippet, "First excerpt\n\nSecond excerpt");
        assert_eq!(hit.published.as_deref(), Some("2025-01-02"));
    }

    #[test]
    fn omits_optional_settings_without_a_limit() {
        let request = Request {
            objective: "rust ownership",
            search_queries: vec!["rust ownership".to_owned()],
            advanced_settings: None,
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["objective"], "rust ownership");
        assert!(json.get("advanced_settings").is_none());
    }
}
