//! What a caller asks for, and what comes back.
//!
//! Provider-neutral by intent: nothing here names SearXNG. A second engine
//! family (an API-key search provider, say) should fit these types without
//! widening them.

/// A search request.
///
/// Built by [`Query::new`] and narrowed with the `with_*` methods; every
/// field beyond the text has a defensible default, so the one-argument form is
/// the common one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    /// The text to search for, as a user would type it.
    pub text: String,
    /// 1-based page number. Page 1 is the default; a caller paging deeply
    /// should expect public instances to get bored before it does.
    pub page: u32,
    /// Result categories (`general`, `news`, `images`, `science`, …). Empty
    /// means the instance's own default.
    pub categories: Vec<String>,
    /// Restrict to named upstream engines (`google`, `duckduckgo`, …). Empty
    /// means the instance's own default. Naming engines an instance hasn't
    /// enabled produces fewer results, not an error.
    pub engines: Vec<String>,
    /// BCP-47-ish language tag as SearXNG spells it (`en`, `en-US`, `all`).
    pub language: Option<String>,
    /// Recency window.
    pub time_range: Option<TimeRange>,
    /// Adult-content filtering.
    pub safe_search: SafeSearch,
    /// Truncate to at most this many hits. Applied here, after the response
    /// arrives — the JSON API has no count parameter to forward.
    pub limit: Option<usize>,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Query {
            text: text.into(),
            page: 1,
            ..Query::default()
        }
    }

    pub fn with_page(mut self, page: u32) -> Self {
        self.page = page.max(1);
        self
    }

    pub fn with_categories<I, S>(mut self, categories: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.categories = categories.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_engines<I, S>(mut self, engines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.engines = engines.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    pub fn with_time_range(mut self, range: TimeRange) -> Self {
        self.time_range = Some(range);
        self
    }

    pub fn with_safe_search(mut self, level: SafeSearch) -> Self {
        self.safe_search = level;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// Adult-content filtering, in SearXNG's three levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SafeSearch {
    #[default]
    Off,
    Moderate,
    Strict,
}

impl SafeSearch {
    pub(crate) fn as_param(self) -> &'static str {
        match self {
            SafeSearch::Off => "0",
            SafeSearch::Moderate => "1",
            SafeSearch::Strict => "2",
        }
    }
}

/// Recency window for a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
}

impl TimeRange {
    pub(crate) fn as_param(self) -> &'static str {
        match self {
            TimeRange::Day => "day",
            TimeRange::Week => "week",
            TimeRange::Month => "month",
            TimeRange::Year => "year",
        }
    }
}

/// One answer to one [`Query`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    /// The query the instance says it ran. Not necessarily byte-identical to
    /// what was sent — instances normalise, and bang-syntax is consumed.
    pub query: String,
    pub hits: Vec<Hit>,
    /// Direct answers (calculator, definitions, unit conversion).
    pub answers: Vec<Answer>,
    /// Alternative spellings the instance proposes.
    pub suggestions: Vec<String>,
    /// Corrections to the query itself, when it thinks it was a typo.
    pub corrections: Vec<String>,
    /// Upstream engines that failed to answer *for this query*. Worth
    /// surfacing: thin results with a long list here mean a degraded instance,
    /// not a thin web.
    pub unresponsive_engines: Vec<String>,
    /// Which instance served this, for attribution and for debugging a pool.
    /// Set by the engine, not by the wire.
    pub source: Option<String>,
}

/// One result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub url: String,
    pub title: String,
    /// The result snippet. Empty when the upstream engine gave none.
    pub snippet: String,
    /// Which upstream engines produced this result. More than one is a
    /// meaningful signal — SearXNG merges duplicates across engines.
    pub engines: Vec<String>,
    /// SearXNG's own merged score. Comparable within one response only.
    pub score: f32,
    pub category: Option<String>,
    /// Publication date as the engine reported it, unparsed.
    pub published: Option<String>,
    pub thumbnail: Option<String>,
}

/// A direct answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub url: Option<String>,
}
