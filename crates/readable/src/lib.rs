//! HTML responses, turned into something an agent can read.
//!
//! One call — [`read`] — takes the bytes of an HTTP response and returns a
//! [`Document`]: metadata plus Markdown, serialisable as JSON. "Markdown or
//! JSON" is a rendering of one extraction, not two pipelines, so the two can
//! never disagree about what the page said.
//!
//! ```no_run
//! # use readable::{Options, read};
//! # use url::Url;
//! # fn run(body: &[u8]) -> Result<(), readable::Error> {
//! let url = Url::parse("https://example.org/post").unwrap();
//! let document = read(body, &url, Some("text/html; charset=utf-8"), &Options::default())?;
//!
//! println!("{}", document.to_markdown()); // for a model to read
//! println!("{}", document.to_json());     // for a program to consume
//! # Ok(())
//! # }
//! ```
//!
//! ## Deliberately absent
//!
//! - **No fetching.** This crate never opens a socket. That is not tidiness:
//!   fetching a URL an agent chose, from a service running many users' work,
//!   is a request to reach arbitrary hosts from inside the network — cloud
//!   metadata endpoints, `localhost` admin ports, internal services. That
//!   needs an explicit allow/deny policy and a decision about who may reach
//!   what, which is a crate with its own design, not a convenience method
//!   bolted onto a converter. This one takes bytes somebody else already has.
//! - **No JavaScript.** A page that assembles itself client-side yields
//!   [`Error::NoContent`] with a note saying so, rather than an empty document
//!   that reads as "this page is blank". Rendering needs a browser.
//! - **No caching, no rate limiting, no retries.** Nothing here has state
//!   between calls.
//! - **No general structured-data extraction.** Metadata is harvested from
//!   `<title>`, OpenGraph, and JSON-LD because it is nearly free and names the
//!   document. Pulling arbitrary fields out of arbitrary pages is a different
//!   ask.
//!
//! ## Two decisions worth knowing about
//!
//! **It takes `&[u8]`, not `&str`.** An HTTP client's `text()` honours the
//! charset in the `Content-Type` header and otherwise assumes UTF-8; it does
//! not read `<meta charset>`. Pages in EUC-KR, Shift_JIS and windows-1252
//! served under a bare `text/html` decode into a wall of `U+FFFD` that way.
//! See [`charset`].
//!
//! **Extraction is a choice, not a default.** Stripping a page down to its
//! article is right for a news post and wrong for a documentation page, where
//! the sidebar is the navigation an agent needs. [`Mode::Auto`] keeps an
//! extraction only if it convinces, falls back to the whole page otherwise,
//! and records which happened. See [`read`].

pub mod charset;
mod clean;
mod document;
mod error;
mod markdown;
mod read;

pub use document::{Document, Extraction, ModeUsed};
pub use error::{Error, Result};
pub use markdown::Truncation;
pub use read::{Mode, Options, read};
