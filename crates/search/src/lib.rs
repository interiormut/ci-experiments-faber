//! Web search for Faber's core.
//!
//! One trait — [`SearchEngine`] — turns a provider-neutral [`Query`] into
//! [`Results`], and two providers implement it:
//!
//! - [`searxng::SearxNg`] talks to **one** SearXNG instance you name. Bring
//!   your own, or point it at an instance you trust.
//! - [`public::PublicSearxNg`] talks to the **public instance network**,
//!   discovered from `searx.space`, spreading queries across many instances so
//!   no one of them is leaned on.
//!
//! Deliberately absent, as in [`llm`](../llm/index.html):
//!
//! - **No caller-level retry policy.** A failure is a value ([`Error`]); when
//!   to ask again is the caller's decision. The one exception is stated below
//!   and is not a policy the caller could supply.
//! - **No result ranking or rewriting.** A [`Hit`] is what the instance said,
//!   normalised in shape only. Deduplication, re-ranking, and summarisation
//!   belong above this crate.
//! - **No fetching of result pages.** Search returns links; reading them is
//!   somebody else's capability.
//! - **No ambient authority.** A client reads no process environment: not for
//!   credentials, not for an instance URL, and — because Faber runs many
//!   users' work in one process — not for proxies either. `reqwest` picks up
//!   `HTTP_PROXY`/`HTTPS_PROXY` by default; this crate turns that off and
//!   takes a proxy only through [`Config::with_proxy`](searxng::Config::with_proxy).
//!
//! ## The one exception: cross-instance failover
//!
//! [`public::PublicSearxNg`] retries a failed query on a *different* instance,
//! up to a small bound. That is not a retry policy sneaking in — it is what
//! "query the public SearXNG network" means. A single public instance is not
//! the service; the network is, and any instance in it may rate-limit,
//! disappear, or answer in HTML at any moment. Retrying the *same* instance is
//! still the caller's call, and this crate never does it.
//!
//! ## Why the public provider is more than a URL list
//!
//! Two facts, measured against the live directory rather than assumed:
//!
//! 1. **SearXNG's JSON API is off by default.** Of 75 reachable public
//!    instances probed from a cold IP, exactly one answered
//!    `format=json` with JSON. The rest answered with the HTML search page
//!    (`200 text/html`), a `403`, or the bot limiter's `429`. So an instance
//!    has to be *probed* before it can be used — and only the first of those,
//!    a success status carrying HTML, is a **permanent** disqualification. The
//!    other two expire, and treating them as verdicts would retire most of the
//!    network on first contact.
//! 2. **Rate limiting is the normal state, not the exception.** Bursting the
//!    one instance that worked last time is the fastest way to lose it.
//!
//! Hence [`public::Scheduler`]: eligibility first (JSON-verified, out of
//! cooldown, not already busy), then **least-recently-used** among the healthy
//! top tier. Score-maximising selection would keep returning the same winner,
//! which is the behaviour to avoid. See that module for the details.
//!
//! ```no_run
//! # use search::{Query, SearchEngine, public, searxng};
//! # async fn run() -> Result<(), search::Error> {
//! // One instance you name.
//! let engine = searxng::SearxNg::new(searxng::Config::new("https://searx.example.org")?)?;
//! let results = engine.search(&Query::new("rust ownership")).await?;
//!
//! // Or the public network, discovered and probed up front.
//! let engine = public::PublicSearxNg::discover(public::Config::default()).await?;
//! let results = engine.search(&Query::new("rust ownership")).await?;
//! for hit in &results.hits {
//!     println!("{} — {}", hit.title, hit.url);
//! }
//! # Ok(())
//! # }
//! ```

mod endpoint;
mod engine;
mod error;
mod http;
pub mod public;
pub mod searxng;
mod types;

pub use engine::SearchEngine;
pub use error::{Error, Result};
pub use types::{Answer, Hit, Query, Results, SafeSearch, TimeRange};
