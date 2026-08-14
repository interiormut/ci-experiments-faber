//! What comes out: one struct that is both the JSON and the Markdown.
//!
//! Deliberately one type rather than two output paths. "Markdown or JSON" is a
//! rendering choice, not two extractions — if they were separate pipelines they
//! would disagree about the title eventually, and the JSON consumer and the
//! Markdown reader would be looking at different pages.

use serde::{Deserialize, Serialize};

use crate::markdown::Truncation;

/// A page, reduced to what an agent can use.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    /// The URL this was read from, as given.
    pub url: String,
    pub title: Option<String>,
    /// The author line, when the page states one.
    pub byline: Option<String>,
    pub site_name: Option<String>,
    /// BCP-47 tag from `<html lang>` or the page's metadata.
    pub language: Option<String>,
    /// Publication time, as the page spelled it — usually ISO 8601, but this
    /// is not parsed or normalised, because a wrong date is worse than a
    /// string.
    pub published: Option<String>,
    /// The page's own summary, from OpenGraph or a meta description.
    pub excerpt: Option<String>,
    /// The content, as Markdown. Already truncated if [`Self::truncation`]
    /// says so, and already carrying the notice.
    pub content: String,
    /// How the content was arrived at.
    pub extraction: Extraction,
}

/// The provenance of [`Document::content`].
///
/// Present in the JSON because "what did you actually do to my page" is a
/// question the caller will have, especially when the answer is "fell back to
/// the whole page because extraction found nothing".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Extraction {
    pub mode: ModeUsed,
    /// Characters of text in the content, before Markdown syntax.
    pub text_chars: usize,
    /// Set when the output was capped.
    pub truncation: Option<Truncation>,
    /// The character encoding the bytes were decoded with.
    pub charset: String,
    /// Whether decoding hit bytes it could not represent. When true, the text
    /// contains `U+FFFD` and the page's declared encoding was probably wrong.
    pub lossy_decode: bool,
}

/// Which stage produced the content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeUsed {
    /// Readability found an article and it was used.
    Article,
    /// The whole page was converted — either because the caller asked for it,
    /// or because extraction did not convince.
    FullPage,
}

impl Document {
    /// Renders the document as Markdown, metadata included.
    ///
    /// The header is what makes the result self-describing: an agent that gets
    /// this text with no other context still knows what it is reading and
    /// where it came from, and can cite it.
    pub fn to_markdown(&self) -> String {
        let mut out = String::with_capacity(self.content.len() + 256);

        if let Some(title) = &self.title {
            out.push_str("# ");
            out.push_str(title);
            out.push_str("\n\n");
        }

        let mut facts = Vec::new();
        if let Some(byline) = &self.byline {
            facts.push(byline.clone());
        }
        if let Some(site) = &self.site_name {
            facts.push(site.clone());
        }
        if let Some(published) = &self.published {
            facts.push(published.clone());
        }
        if !facts.is_empty() {
            out.push('*');
            out.push_str(&facts.join(" · "));
            out.push_str("*\n\n");
        }

        out.push_str("Source: <");
        out.push_str(&self.url);
        out.push_str(">\n\n---\n\n");
        out.push_str(&self.content);
        out
    }

    /// Renders the document as JSON.
    pub fn to_json(&self) -> String {
        // The struct is plain data with no map keys that can fail to
        // serialise, so this cannot error in practice.
        serde_json::to_string_pretty(self).unwrap_or_else(|error| {
            format!("{{\"error\":\"could not serialise document: {error}\"}}")
        })
    }

    /// Whether the output was capped.
    pub fn is_truncated(&self) -> bool {
        self.extraction.truncation.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Document {
        Document {
            url: "https://example.org/post".into(),
            title: Some("A Title".into()),
            byline: Some("A. Writer".into()),
            site_name: Some("Example".into()),
            language: Some("en".into()),
            published: Some("2026-01-02".into()),
            excerpt: Some("A summary.".into()),
            content: "Body text.".into(),
            extraction: Extraction {
                mode: ModeUsed::Article,
                text_chars: 10,
                truncation: None,
                charset: "UTF-8".into(),
                lossy_decode: false,
            },
        }
    }

    #[test]
    fn markdown_is_self_describing() {
        let markdown = document().to_markdown();
        assert!(markdown.starts_with("# A Title"));
        assert!(markdown.contains("*A. Writer · Example · 2026-01-02*"));
        assert!(markdown.contains("Source: <https://example.org/post>"));
        assert!(markdown.ends_with("Body text."));
    }

    #[test]
    fn a_page_with_no_metadata_still_renders() {
        let mut document = document();
        document.title = None;
        document.byline = None;
        document.site_name = None;
        document.published = None;
        let markdown = document.to_markdown();
        assert!(markdown.starts_with("Source: <"));
        assert!(markdown.contains("Body text."));
    }

    #[test]
    fn json_round_trips() {
        let json = document().to_json();
        let parsed: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title.as_deref(), Some("A Title"));
        assert_eq!(parsed.extraction.mode, ModeUsed::Article);
        assert!(json.contains("\"mode\": \"article\""));
    }
}
