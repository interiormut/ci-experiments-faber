//! Preparing the tree, before anything decides what to keep.
//!
//! Two jobs, both done once on the parsed document so that article extraction
//! and whole-page conversion inherit them equally.
//!
//! **Removing what is not content.** Scripts, styles, and inline SVG are not
//! prose, and one inline `<svg>` of chart paths can outweigh the article it
//! decorates. This is a budget concern before it is a tidiness one: everything
//! left here becomes tokens.
//!
//! **Resolving links.** An agent handed `/docs/install` cannot follow it — it
//! has no idea what host it came from. Every `href` and `src` is resolved
//! against `<base href>` if the document declares one and the page URL
//! otherwise, which is why a URL is a required input to a crate that does no
//! fetching.

use dom_query::Document;
use url::Url;

/// Elements removed outright: they carry no text an agent wants, and several
/// carry enormous amounts of text it does not.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "svg", "canvas", "template", "iframe", "object", "embed",
    "link", "form", "input", "select", "textarea", "button",
];

/// Strips non-content elements from the tree.
///
/// **Must run after metadata has been harvested.** A page's byline, site name
/// and publication date usually arrive as JSON-LD, which lives inside a
/// `<script type="application/ld+json">` — so calling this first throws away
/// the metadata along with the machinery, and the loss is silent.
pub fn strip(document: &Document) {
    for selector in DROPPED {
        document.select(selector).remove();
    }
}

/// Rewrites relative URLs in place, and drops the ones nothing can follow.
///
/// `base` is the page's own URL; a `<base href>` in the document overrides it,
/// which is the rule browsers follow and the reason a page can be served from
/// one path and link as though it were at another.
pub fn resolve_urls(document: &Document, page_url: &Url) {
    let base = document
        .select("base[href]")
        .attr("href")
        .and_then(|href| page_url.join(href.as_ref()).ok())
        .unwrap_or_else(|| page_url.clone());

    for (selector, attribute) in [
        ("a[href]", "href"),
        ("area[href]", "href"),
        ("img[src]", "src"),
        ("source[src]", "src"),
        ("video[src]", "src"),
        ("audio[src]", "src"),
    ] {
        for node in document.select(selector).iter() {
            let Some(value) = node.attr(attribute) else {
                continue;
            };
            let value = value.trim();

            match classify(value) {
                Target::Absolute => {}
                Target::Relative => {
                    if let Ok(resolved) = base.join(value) {
                        node.set_attr(attribute, resolved.as_str());
                    } else {
                        node.remove_attr(attribute);
                    }
                }
                Target::Unfollowable => {
                    // A `javascript:` link goes nowhere an agent can go, and a
                    // `data:` image is frequently a screenful of base64 that
                    // would be spent on nothing. Drop the attribute rather
                    // than the element: the link text is often still content.
                    node.remove_attr(attribute);
                }
            }
        }
    }
}

enum Target {
    Absolute,
    Relative,
    Unfollowable,
}

fn classify(value: &str) -> Target {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("data:") || lower.starts_with("about:")
    {
        return Target::Unfollowable;
    }
    if Url::parse(value).is_ok() {
        return Target::Absolute;
    }
    Target::Relative
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        Url::parse("https://example.org/docs/guide/index.html").unwrap()
    }

    #[test]
    fn relative_links_become_followable() {
        let document = Document::from(
            r#"<a href="../install">i</a><a href="/about">a</a><img src="img/x.png">"#,
        );
        resolve_urls(&document, &page());
        let html = document.html().to_string();
        assert!(html.contains("https://example.org/docs/install"));
        assert!(html.contains("https://example.org/about"));
        assert!(html.contains("https://example.org/docs/guide/img/x.png"));
    }

    #[test]
    fn a_base_href_overrides_the_page_url() {
        let document = Document::from(
            r#"<html><head><base href="https://cdn.example.net/v2/"></head><body><a href="x">x</a></body></html>"#,
        );
        resolve_urls(&document, &page());
        assert!(document.html().contains("https://cdn.example.net/v2/x"));
    }

    #[test]
    fn absolute_urls_are_left_alone() {
        let document = Document::from(r#"<a href="https://other.example/p?q=1#f">x</a>"#);
        resolve_urls(&document, &page());
        assert!(document.html().contains("https://other.example/p?q=1#f"));
    }

    /// A single inline base64 image can be larger than the article it sits in,
    /// and no agent can do anything with it.
    #[test]
    fn data_uris_and_script_links_are_dropped_but_their_text_survives() {
        let document = Document::from(
            r#"<img src="data:image/png;base64,AAAA"><a href="javascript:go()">click here</a>"#,
        );
        resolve_urls(&document, &page());
        let html = document.html().to_string();
        assert!(!html.contains("base64"));
        assert!(!html.contains("javascript:"));
        assert!(html.contains("click here"));
    }

    #[test]
    fn strip_removes_scripts_styles_and_inline_svg() {
        let document = Document::from(
            "<p>keep</p><script>var a = 'drop me'</script><style>.x{color:red}</style>\
             <svg><path d='M0 0 L100 100'/></svg><noscript>drop</noscript>",
        );
        strip(&document);
        let html = document.html().to_string();
        assert!(html.contains("keep"));
        for gone in ["drop me", "color:red", "M0 0", "<noscript>"] {
            assert!(!html.contains(gone), "{gone} survived");
        }
    }

    #[test]
    fn a_malformed_href_loses_the_attribute_not_the_text() {
        let document = Document::from(r#"<a href="ht tp://%%%">label</a>"#);
        resolve_urls(&document, &page());
        assert!(document.html().contains("label"));
    }
}
