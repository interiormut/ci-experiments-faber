//! Failure as ordinary values.
//!
//! The interesting failures here are the ones that *look* like success. A
//! JavaScript-rendered page parses perfectly and yields an empty body; a
//! search-results page runs through article extraction and yields forty words
//! of navigation furniture. Both would serialise to a valid [`Document`] that
//! tells an agent the page said nothing — which is a lie it cannot check. So
//! they are errors, not empty documents.
//!
//! [`Document`]: crate::Document

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The response carried no readable text.
    ///
    /// Usually a page whose content is assembled by JavaScript: the HTML is
    /// a shell around an empty mount point, and no amount of parsing will find
    /// prose in it. Reported rather than returned as an empty document because
    /// "this page has no content" and "this page needs a browser" are
    /// different facts and only the caller can act on the second.
    #[error(
        "no readable content: {text_chars} characters of text in {html_bytes} bytes of HTML{}",
        if *.likely_needs_javascript { " (the page appears to render client-side)" } else { "" }
    )]
    NoContent {
        text_chars: usize,
        html_bytes: usize,
        likely_needs_javascript: bool,
    },

    /// The bytes were not HTML.
    ///
    /// Only ever reported when the caller supplied a content type that says
    /// so; without one, anything is parsed as best it can be. A PDF run
    /// through an HTML parser produces plausible-looking garbage, which is
    /// worse than a refusal.
    #[error("not an HTML document: content type is `{content_type}`")]
    NotHtml { content_type: String },

    /// The input was larger than [`Options::max_input_bytes`] allows.
    ///
    /// A bound on *our* work, unrelated to output truncation: parsing a
    /// 200 MB response to then throw most of it away is a way for one user's
    /// request to spend everyone's memory.
    ///
    /// [`Options::max_input_bytes`]: crate::Options::max_input_bytes
    #[error("document is {bytes} bytes, over the {limit} byte limit")]
    TooLarge { bytes: usize, limit: usize },

    /// The page URL was not usable as a base for resolving links.
    #[error("invalid document URL: {0}")]
    Url(String),

    /// The HTML parsed but the Markdown writer could not render it.
    #[error("could not render Markdown: {0}")]
    Render(String),
}

pub type Result<T> = std::result::Result<T, Error>;
