//! The pipeline.
//!
//! One parse, then a fork:
//!
//! ```text
//! bytes ─► decode ─► parse ─► clean ─► metadata ─┬─► article ──┐
//!                                                └─► whole page┴─► Markdown ─► cap
//! ```
//!
//! ## Why extraction is a choice and not a default
//!
//! Readability-style extraction throws away navigation, sidebars, headers and
//! footers to find the article. That is exactly right for a news page and
//! exactly wrong for an API reference, where the sidebar *is* the index an
//! agent needs in order to find anything — and for a page that is mostly
//! tables, and for a search-results page that has no article at all.
//!
//! Measured, on the standard library's page for `u8`: extraction keeps 120
//! links to methods and the whole page has 1719. The other 1599 are the
//! sidebar index — the part an agent would use to answer "what else can this
//! type do", thrown away as furniture.
//!
//! So [`Mode::Article`] and [`Mode::FullPage`] are both first-class, and
//! [`Mode::Auto`] — the default — runs extraction and keeps it only if it
//! convinces. Whichever ran is recorded in the output, because a caller
//! comparing two pages needs to know whether they were treated the same way.

use dom_query::Document as Dom;
use dom_smoothie::{Config, Readability};
use url::Url;

use crate::document::{Document, Extraction, ModeUsed};
use crate::error::{Error, Result};
use crate::{charset, clean, markdown};

/// Which part of the page to convert.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Extract the article; fall back to the whole page when extraction does
    /// not convince. The right default for "read me this link".
    #[default]
    Auto,
    /// Always extract. Returns [`Error::NoContent`] rather than falling back,
    /// for a caller that wants prose or nothing.
    Article,
    /// Convert the whole body. What documentation, reference tables, and
    /// index pages need.
    FullPage,
}

/// How to read a page.
#[derive(Clone, Debug)]
pub struct Options {
    pub mode: Mode,
    /// Cap on the Markdown, in bytes. Zero means no cap.
    ///
    /// The default is sized for a page an agent reads in one go — a large
    /// reference page converts to several hundred kilobytes, which is tens of
    /// thousands of tokens spent before it has read a word.
    pub max_output_bytes: usize,
    /// Refuse input larger than this rather than parsing it. Zero means no
    /// limit.
    pub max_input_bytes: usize,
    /// Text below this many characters counts as no content at all.
    ///
    /// The floor that catches client-rendered pages, which parse perfectly
    /// into an empty shell.
    pub min_content_chars: usize,
    /// In [`Mode::Auto`], the extracted article must hold at least this
    /// fraction of the page's text to be preferred over the whole page.
    ///
    /// A backstop, deliberately set low. Across the real pages in this crate's
    /// test corpus the extracted article runs from 44% of the page's text (an
    /// API reference, where the sidebar is most of the page) to 99% (a
    /// documentation chapter), so this never decides anything there;
    /// [`Self::min_content_chars`] is what does the work.
    ///
    /// It exists for the case that floor cannot see — extraction latching onto
    /// a paragraph on a page that is otherwise entirely navigation — where
    /// returning the whole page is the lesser mistake. That case is
    /// constructed in this module's tests, because no page in the corpus
    /// produces it naturally.
    pub min_article_ratio: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::default(),
            max_output_bytes: 128 * 1024,
            max_input_bytes: 32 * 1024 * 1024,
            min_content_chars: 200,
            min_article_ratio: 0.05,
        }
    }
}

impl Options {
    pub fn article() -> Self {
        Options {
            mode: Mode::Article,
            ..Options::default()
        }
    }

    pub fn full_page() -> Self {
        Options {
            mode: Mode::FullPage,
            ..Options::default()
        }
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

/// Reads an HTTP response body into a [`Document`].
///
/// `url` is the address the bytes came from. It is required even though
/// nothing here fetches: relative links resolve against it, and a document
/// full of `/docs/next` is one an agent cannot navigate.
///
/// `content_type` is the response's `Content-Type` header, when there is one.
/// It settles the character encoding, and it is the only thing that can prove
/// the bytes are not HTML at all.
pub fn read(
    bytes: &[u8],
    url: &Url,
    content_type: Option<&str>,
    options: &Options,
) -> Result<Document> {
    if options.max_input_bytes > 0 && bytes.len() > options.max_input_bytes {
        return Err(Error::TooLarge {
            bytes: bytes.len(),
            limit: options.max_input_bytes,
        });
    }
    if let Some(content_type) = content_type
        && !charset::is_html(content_type)
    {
        return Err(Error::NotHtml {
            content_type: content_type.to_owned(),
        });
    }

    let decoded = charset::decode(bytes, content_type);
    let html_bytes = decoded.text.len();

    let dom = Dom::from(decoded.text.as_str());
    // Asked before stripping, which removes the evidence.
    let client_rendered = is_client_rendered(&dom);
    clean::resolve_urls(&dom, url);

    // Metadata comes off the untouched tree, and this ordering is not
    // cosmetic. A page's byline, site name and publication date usually live
    // in JSON-LD — which is to say, inside a `<script type="application/ld+json">`
    // — so stripping scripts first silently throws them away. Measured on
    // Wikipedia, cleaning before harvesting turned the byline from the page's
    // actual credit into the caption of a navigation box.
    //
    // Both modes read metadata from here, so they cannot disagree about who
    // wrote the page.
    let mut readability =
        Readability::with_document(dom, Some(url.as_str()), Some(config(options)))
            .map_err(|error| Error::Url(error.to_string()))?;
    let mut meta = Meta::from_page(readability.get_article_metadata(readability.parse_json_ld()));

    // Now that nothing else needs them, the non-content elements go.
    clean::strip(&readability.doc);

    // The whole-page text, measured once: the floor that catches a
    // client-rendered shell, and the denominator for judging extraction.
    let page_text_chars = text_chars(&readability.doc.select("body").text());
    let page_html = readability.doc.select("body").html().to_string();

    let refused = |chars| Err(no_content(chars, html_bytes, client_rendered));
    let (content_html, mode_used, text_chars_used) = match options.mode {
        Mode::FullPage => (page_html, ModeUsed::FullPage, page_text_chars),
        Mode::Article => {
            let Ok(article) = readability.parse() else {
                return refused(page_text_chars);
            };
            let article_chars = text_chars(&article.text_content);
            if article_chars < options.min_content_chars {
                return refused(article_chars);
            }
            meta.enrich(&article);
            (
                article.content.to_string(),
                ModeUsed::Article,
                article_chars,
            )
        }
        Mode::Auto => match readability.parse() {
            Ok(article) if convincing(&article, page_text_chars, options) => {
                let chars = text_chars(&article.text_content);
                meta.enrich(&article);
                (article.content.to_string(), ModeUsed::Article, chars)
            }
            // Extraction produced a scrap, or nothing at all. The whole page
            // is a worse read than a good extraction and a far better one
            // than forty words of navigation furniture.
            _ => {
                tracing::debug!(
                    url = %url,
                    page_text_chars,
                    "article extraction did not convince; converting the whole page"
                );
                (page_html, ModeUsed::FullPage, page_text_chars)
            }
        },
    };

    if text_chars_used < options.min_content_chars {
        return refused(text_chars_used);
    }

    let converted = markdown::convert(&content_html)?;
    let (content, truncation) = markdown::truncate(converted, options.max_output_bytes);

    Ok(Document {
        url: url.to_string(),
        title: meta.title,
        byline: meta.byline,
        site_name: meta.site_name,
        language: meta.language,
        published: meta.published,
        excerpt: meta.excerpt,
        content,
        extraction: Extraction {
            mode: mode_used,
            text_chars: text_chars_used,
            truncation,
            charset: decoded.charset.to_owned(),
            lossy_decode: decoded.had_errors,
        },
    })
}

/// Whether an extracted article is worth preferring over the whole page.
///
/// Two ways to fail: too little text to be an article at all, or a share of
/// the page so small that extraction has evidently latched onto a fragment.
///
/// Both sides are measured with [`text_chars`] rather than using
/// `Article::length`, which counts raw characters including the indentation
/// whitespace between tags. Mixing the two makes the ratio a comparison of
/// different units — and inflates it enough that a heavily indented page can
/// report an "article" longer than the page it came from.
fn convincing(article: &dom_smoothie::Article, page_text_chars: usize, options: &Options) -> bool {
    let article_chars = text_chars(&article.text_content);
    if article_chars < options.min_content_chars {
        return false;
    }
    if page_text_chars == 0 {
        return true;
    }
    article_chars as f32 / page_text_chars as f32 >= options.min_article_ratio
}

/// Readability's own tuning is left alone.
///
/// Lowering its `char_threshold` to let this crate's floor decide instead was
/// tried and made things worse: a lower bar makes extraction accept weaker
/// candidates, and the metadata that comes back describes the weaker candidate
/// — on Wikipedia it turned the byline from the page's actual credit into the
/// caption of a navigation box at the foot of the article. The threshold is
/// load-bearing for extraction *quality*, not just for its verdict.
///
/// [`Options::min_content_chars`] therefore governs the final content, and
/// this governs whether an article is found at all.
fn config(_options: &Options) -> Config {
    Config::default()
}

/// The metadata that names a document, from whichever source knew it.
///
/// Page-level metadata — OpenGraph, JSON-LD, meta tags — wins wherever it
/// exists, and extraction fills only the gaps. That order is the opposite of
/// the tempting one and it was chosen by measurement: page metadata is what
/// the publisher *declared*, while extraction's byline is a guess made by
/// scoring elements, and on Wikipedia that guess lands on the caption of a
/// navigation box. Where the publisher declared nothing, though, the guess is
/// all there is and it is often right — the Rust blog states its author only
/// in the article body.
#[derive(Debug, Default)]
struct Meta {
    title: Option<String>,
    byline: Option<String>,
    site_name: Option<String>,
    language: Option<String>,
    published: Option<String>,
    excerpt: Option<String>,
}

impl Meta {
    fn from_page(metadata: dom_smoothie::Metadata) -> Self {
        Meta {
            title: non_empty(metadata.title),
            byline: metadata.byline.and_then(non_empty),
            site_name: metadata.site_name.and_then(non_empty),
            language: metadata.lang.and_then(non_empty),
            published: metadata.published_time.and_then(non_empty),
            excerpt: metadata.excerpt.and_then(non_empty),
        }
    }

    /// Fills the gaps from an extracted article. Never overwrites a value the
    /// page declared for itself.
    fn enrich(&mut self, article: &dom_smoothie::Article) {
        fill(&mut self.title, non_empty(article.title.clone()));
        fill(&mut self.byline, article.byline.clone().and_then(non_empty));
        fill(
            &mut self.site_name,
            article.site_name.clone().and_then(non_empty),
        );
        fill(&mut self.language, article.lang.clone().and_then(non_empty));
        fill(
            &mut self.published,
            article.published_time.clone().and_then(non_empty),
        );
        fill(
            &mut self.excerpt,
            article.excerpt.clone().and_then(non_empty),
        );
    }
}

fn fill(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none()
        && let Some(value) = value
    {
        *slot = Some(value);
    }
}

/// Empty containers that mean "a framework will fill this in".
const MOUNT_POINTS: &[&str] = &[
    "#root",
    "#app",
    "#__next",
    "#__nuxt",
    "#application",
    "[data-reactroot]",
    "[data-server-rendered]",
    ".application-main > .js-render-target",
];

/// Whether an empty page is empty *because it has not been rendered yet*.
///
/// Asked of the tree before cleaning strips the evidence. The tempting signal
/// — does the page have scripts — carries no information: of the real pages in
/// the fixture corpus, every single one has script tags and most have a
/// `<noscript>` as well, so "has scripts" is true of the whole web and would
/// make this flag advice to fetch a headless browser for every paywall, login
/// wall and image gallery.
///
/// What does discriminate is the page saying so. Either it says it in words,
/// in a `<noscript>` that asks for JavaScript to be enabled, or it says it in
/// structure, with a mount point a framework is expected to fill and has not.
fn is_client_rendered(dom: &Dom) -> bool {
    for node in dom.select("noscript").iter() {
        let text = node.text().to_lowercase();
        if text.contains("javascript") || text.contains("enable js") {
            return true;
        }
    }
    MOUNT_POINTS.iter().any(|selector| {
        dom.select(selector)
            .iter()
            .any(|node| node.text().trim().is_empty())
    })
}

/// A page that parsed but said nothing.
///
/// The client-rendered guess is a heuristic and says so in the message; a page
/// of nothing but images would look the same and is not something rendering
/// would fix.
fn no_content(text_chars: usize, html_bytes: usize, client_rendered: bool) -> Error {
    Error::NoContent {
        text_chars,
        html_bytes,
        likely_needs_javascript: client_rendered,
    }
}

/// Counts characters of actual text, ignoring the whitespace that formatting
/// leaves between tags.
fn text_chars(text: &str) -> usize {
    let mut characters = 0usize;
    let mut words = 0usize;
    for word in text.split_whitespace() {
        characters += word.chars().count();
        words += 1;
    }
    // One separating space between words, as a collapsed rendering would have.
    characters + words.saturating_sub(1)
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        Url::parse("https://example.org/page").unwrap()
    }

    #[test]
    fn text_chars_measures_a_collapsed_rendering() {
        assert_eq!(text_chars("  one   two\n\nthree  "), "one two three".len());
        assert_eq!(text_chars(""), 0);
        assert_eq!(text_chars("   \n  "), 0);
        // Characters, not bytes: Korean is three bytes each.
        assert_eq!(text_chars("한국어"), 3);
    }

    /// The backstop `min_article_ratio` exists for, built deliberately because
    /// no page in the fixture corpus triggers it: a wall of navigation with a
    /// paragraph in it. Extraction finds the paragraph, which clears the
    /// character floor — and returning it alone would throw away the 97% of
    /// the page that is actually there.
    #[test]
    fn a_tiny_article_on_a_huge_page_falls_back_to_the_whole_page() {
        let navigation: String = (0..900)
            .map(|i| {
                format!(
                    "<li><a href=\"/section/{i}\">Section number {i} of the site index</a></li>"
                )
            })
            .collect();
        // Long enough that extraction accepts it — its own floor is higher
        // than this crate's — and still a rounding error against the index.
        let article = "This is the only paragraph of prose on the page. It is long \
                       enough that article extraction is willing to call it an \
                       article, which is the point: the character floor cannot \
                       reject it, so something else has to notice that it is a \
                       scrap of a page which is otherwise entirely navigation. \
                       It goes on for a while so that it clears every threshold \
                       that measures absolute length rather than proportion. "
            .repeat(2);
        let html = format!(
            "<html><body><nav><ul>{navigation}</ul></nav><article><p>{article}</p></article></body></html>"
        );

        let with_backstop = read(html.as_bytes(), &page(), None, &Options::default()).unwrap();
        assert_eq!(
            with_backstop.extraction.mode,
            ModeUsed::FullPage,
            "a paragraph in a wall of links is not an article"
        );

        // Turn the backstop off and the same page extracts to the scrap,
        // which is what it is there to prevent.
        let without = read(
            html.as_bytes(),
            &page(),
            None,
            &Options {
                min_article_ratio: 0.0,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(without.extraction.mode, ModeUsed::Article);
        assert!(without.extraction.text_chars < with_backstop.extraction.text_chars / 10);
    }
}
