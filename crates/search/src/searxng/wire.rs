//! SearXNG's `format=json` response, as it actually arrives.
//!
//! Tolerant on purpose. The JSON API is stable in outline and unstable in
//! detail across the versions public instances run — `answers` has been both a
//! list of strings and a list of objects, `unresponsive_engines` is a list of
//! *arrays*, and result objects grow fields per template. Every field is
//! `#[serde(default)]` and unknown fields are ignored, so a new SearXNG
//! release cannot turn a working instance into a decode error.

use serde::Deserialize;

use crate::types::{Answer, Hit, Results};

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Response {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub results: Vec<WireHit>,
    #[serde(default)]
    pub answers: Vec<WireAnswer>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub corrections: Vec<String>,
    /// Arrives as `[["engine name", "reason"], …]` on current releases and as
    /// `["engine name", …]` on older ones.
    #[serde(default)]
    pub unresponsive_engines: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireHit {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    engines: Vec<String>,
    #[serde(default)]
    score: f32,
    #[serde(default)]
    category: Option<String>,
    #[serde(rename = "publishedDate", default)]
    published_date: Option<String>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    img_src: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum WireAnswer {
    Text(String),
    Object {
        #[serde(default)]
        answer: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
}

impl Response {
    pub(crate) fn into_results(self, limit: Option<usize>) -> Results {
        let mut hits: Vec<Hit> = self
            .results
            .into_iter()
            .filter_map(WireHit::into_hit)
            .collect();
        if let Some(limit) = limit {
            hits.truncate(limit);
        }

        Results {
            query: self.query,
            hits,
            answers: self
                .answers
                .into_iter()
                .filter_map(WireAnswer::into_answer)
                .collect(),
            suggestions: self.suggestions,
            corrections: self.corrections,
            unresponsive_engines: self
                .unresponsive_engines
                .iter()
                .filter_map(engine_name)
                .collect(),
            source: None,
        }
    }
}

impl WireHit {
    /// `None` for entries with no URL.
    ///
    /// Not every element of `results` is a link: image and map templates put
    /// entries there whose only address is a thumbnail, and a caller handed a
    /// [`Hit`] with an empty `url` has been handed a bug rather than a result.
    fn into_hit(self) -> Option<Hit> {
        let url = self.url.filter(|u| !u.is_empty())?;
        // `engines` is the merged list and `engine` the single producer;
        // instances have shipped either, so prefer the list and fall back.
        let engines = if self.engines.is_empty() {
            self.engine.into_iter().collect()
        } else {
            self.engines
        };
        Some(Hit {
            title: self.title.unwrap_or_default(),
            snippet: self.content.unwrap_or_default(),
            url,
            engines,
            score: self.score,
            category: self.category.filter(|c| !c.is_empty()),
            published: self.published_date.filter(|p| !p.is_empty()),
            thumbnail: self
                .thumbnail
                .or(self.img_src)
                .filter(|t: &String| !t.is_empty()),
        })
    }
}

impl WireAnswer {
    fn into_answer(self) -> Option<Answer> {
        match self {
            WireAnswer::Text(text) if !text.is_empty() => Some(Answer { text, url: None }),
            WireAnswer::Object { answer, url } => {
                answer.filter(|a| !a.is_empty()).map(|text| Answer {
                    text,
                    url: url.filter(|u| !u.is_empty()),
                })
            }
            WireAnswer::Text(_) => None,
        }
    }
}

fn engine_name(entry: &serde_json::Value) -> Option<String> {
    match entry {
        serde_json::Value::String(name) => Some(name.clone()),
        serde_json::Value::Array(parts) => parts.first()?.as_str().map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a live instance, trimmed to one result.
    const SAMPLE: &str = r#"{
      "query": "rust lang",
      "results": [{
        "url": "https://rust-lang.org/",
        "title": "Rust Programming Language",
        "content": "Rust is blazingly fast and memory-efficient",
        "engine": "bing",
        "template": "default.html",
        "parsed_url": ["https", "rust-lang.org", "/", "", "", ""],
        "img_src": "",
        "thumbnail": "",
        "priority": "",
        "engines": ["bing"],
        "positions": [1],
        "score": 1.0,
        "category": "general",
        "publishedDate": null
      }],
      "answers": [],
      "corrections": [],
      "infoboxes": [],
      "suggestions": [],
      "unresponsive_engines": []
    }"#;

    #[test]
    fn decodes_a_live_response() {
        let response: Response = serde_json::from_str(SAMPLE).unwrap();
        let results = response.into_results(None);
        assert_eq!(results.query, "rust lang");
        assert_eq!(results.hits.len(), 1);
        let hit = &results.hits[0];
        assert_eq!(hit.url, "https://rust-lang.org/");
        assert_eq!(hit.engines, ["bing"]);
        assert_eq!(hit.category.as_deref(), Some("general"));
        // Empty strings on the wire are absence, not content.
        assert!(hit.thumbnail.is_none());
        assert!(hit.published.is_none());
    }

    #[test]
    fn survives_an_empty_object() {
        let response: Response = serde_json::from_str("{}").unwrap();
        assert!(response.into_results(None).hits.is_empty());
    }

    #[test]
    fn accepts_both_answer_spellings() {
        let response: Response = serde_json::from_str(
            r#"{"answers": ["four", {"answer": "4", "url": "https://example.org"}, {"answer": ""}]}"#,
        )
        .unwrap();
        let answers = response.into_results(None).answers;
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].text, "four");
        assert_eq!(answers[1].url.as_deref(), Some("https://example.org"));
    }

    #[test]
    fn accepts_both_unresponsive_spellings() {
        let response: Response = serde_json::from_str(
            r#"{"unresponsive_engines": [["google", "timeout"], "brave", 7]}"#,
        )
        .unwrap();
        assert_eq!(
            response.into_results(None).unresponsive_engines,
            ["google", "brave"]
        );
    }

    #[test]
    fn drops_results_without_a_url_and_applies_the_limit() {
        let response: Response = serde_json::from_str(
            r#"{"results": [{"url": ""}, {"url": "https://a"}, {"url": "https://b"}]}"#,
        )
        .unwrap();
        let results = response.into_results(Some(1));
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].url, "https://a");
    }

    #[test]
    fn ignores_fields_it_has_never_seen() {
        let response: Response =
            serde_json::from_str(r#"{"query": "x", "brand_new_field": {"a": 1}}"#).unwrap();
        assert_eq!(response.query, "x");
    }
}
