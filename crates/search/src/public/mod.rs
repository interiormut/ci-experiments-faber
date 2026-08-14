//! The public SearXNG network as one search engine.
//!
//! Instances come from `searx.space`, are probed for JSON support, and are
//! then rotated through so no single volunteer's server carries the traffic.
//! The protocol is [`crate::searxng`]'s — this module adds discovery,
//! probing, scheduling, and cross-instance failover, and reimplements none of
//! the wire.
//!
//! ## The shape of the problem
//!
//! Measured against the live directory rather than assumed:
//!
//! - Of 75 reachable public instances, one answered `format=json` with JSON.
//!   The others served the HTML search page under a `200`, a `403`, or the bot
//!   limiter's `429`. **JSON support has to be probed and cannot be predicted
//!   from the directory**, which carries no field for it.
//! - Rate limiting is the ordinary state of affairs, not an exception, so the
//!   pool is built to spread load rather than to find a favourite.
//!
//! That is why construction is [`PublicSearxNg::discover`] — an async
//! constructor that fetches, probes, and only then hands back an engine. A
//! synchronous `new` would have to lie about being ready.
//!
//! ## Rate limiting is not free to discover
//!
//! Probing costs the instance a real search, so probes are bounded
//! ([`Config::probe_batch`], [`Config::target_verified`]) and a probe result is
//! never handed to a caller as if it were their answer.

pub mod directory;
mod scheduler;

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::engine::SearchEngine;
use crate::error::{Error, Result};
use crate::searxng::{DEFAULT_USER_AGENT, SearxNg};
use crate::types::{Query, Results};

pub use directory::{Candidate, DEFAULT_DIRECTORY_TTL, DIRECTORY_URL};
pub use scheduler::{Census, Health, Instance, Lease, Outcome, Policy, Scheduler};

/// How to build a pool.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the instance list comes from. Overridable so a deployment can
    /// pin a mirror or a vetted list of its own.
    pub directory_url: String,
    /// How long a fetched directory is reused before re-fetching.
    pub directory_ttl: Duration,
    pub user_agent: String,
    /// Per-request timeout. Shorter than a single-instance default would be:
    /// with a pool, moving on beats waiting.
    pub timeout: Duration,
    /// Explicit proxy. Never read from the environment — see [`crate::http`].
    pub proxy: Option<String>,
    pub policy: Policy,
    /// Verified instances to aim for before serving, and to top up to later.
    ///
    /// Must exceed [`Policy::tier`]'s useful minimum by enough that a couple
    /// of simultaneous cooldowns do not empty the pool.
    pub target_verified: usize,
    /// Instances probed at once. Probing is cheap for us and a real search for
    /// them, so this is a politeness limit as much as a concurrency one.
    pub probe_batch: usize,
    /// Ceiling on probes per replenishment, across all batches. Bounds the
    /// worst case where most of the directory does not serve JSON.
    pub max_probes: usize,
    /// Instances one query may be tried against before giving up.
    ///
    /// This is failover, not retry: each attempt goes to a *different*
    /// instance. Retrying the same one is the caller's business.
    ///
    /// It also sets the worst case a caller can wait, which matters in a
    /// service serving many users: roughly
    /// `max_attempts × (wait_for_instance + timeout)`, plus one probe sweep.
    /// The defaults put that near a minute; a caller that needs a tighter
    /// bound should lower these rather than wrap the call in a timeout, so the
    /// scheduler still learns how each attempt ended.
    pub max_attempts: usize,
    /// Shortest gap between probe sweeps.
    ///
    /// Without it, every query arriving to a busy pool would start another
    /// sweep, and cooldowns expiring would keep supplying instances to probe —
    /// trading the old failure of burning the network on first contact for the
    /// new one of hammering it steadily. Discovery is not urgent; it can wait.
    pub replenish_interval: Duration,
    /// The query probes use. Something short and cacheable upstream.
    pub probe_query: String,
    /// How long a query waits for a busy or cooling-down instance to free up
    /// before reporting [`Error::NoInstance`]. Zero disables the wait.
    pub wait_for_instance: Duration,
}

/// How often [`PublicSearxNg::wait_for_instance`] re-checks. Small enough not
/// to add noticeable latency to a query that only just missed its turn.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

impl Default for Config {
    fn default() -> Self {
        Config {
            directory_url: DIRECTORY_URL.to_owned(),
            directory_ttl: DEFAULT_DIRECTORY_TTL,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            timeout: Duration::from_secs(12),
            proxy: None,
            policy: Policy::default(),
            target_verified: 6,
            probe_batch: 8,
            max_probes: 48,
            max_attempts: 3,
            replenish_interval: Duration::from_secs(30),
            probe_query: "searxng".to_owned(),
            wait_for_instance: Duration::from_secs(5),
        }
    }
}

impl Config {
    pub fn with_directory_url(mut self, url: impl Into<String>) -> Self {
        self.directory_url = url.into();
        self
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn with_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_target_verified(mut self, target: usize) -> Self {
        self.target_verified = target.max(1);
        self
    }
}

/// A search engine backed by many public instances.
pub struct PublicSearxNg {
    http: Client,
    config: Config,
    scheduler: Arc<Scheduler>,
    /// Serialises directory refresh and probing. Held across awaits on
    /// purpose: without it, a burst of first queries would each probe the same
    /// unverified instances, which is the bursting this crate exists to avoid.
    replenish: Mutex<Replenish>,
}

#[derive(Debug)]
struct Replenish {
    directory_fetched_at: Option<Instant>,
    /// When the last probe sweep started. `None` until the first one.
    probed_at: Option<Instant>,
}

impl PublicSearxNg {
    /// Fetches the directory, probes for JSON support, and returns a pool
    /// ready to serve.
    ///
    /// Fails if no instance could be verified. Returning an engine that cannot
    /// answer anything would only move the failure to the first query, where
    /// it is harder to attribute.
    pub async fn discover(config: Config) -> Result<Self> {
        let http =
            crate::http::client(&config.user_agent, config.timeout, config.proxy.as_deref())?;
        let candidates = directory::fetch(&http, &config.directory_url).await?;
        let engine = Self::from_candidates(config, http, candidates, Some(Instant::now()));
        engine.replenish_now().await?;
        Ok(engine)
    }

    /// Builds a pool from an instance list you already have.
    ///
    /// The seam for a deployment with its own vetted instances, and for tests:
    /// nothing here touches the network until a query does.
    pub fn from_instances(config: Config, instances: Vec<url::Url>) -> Result<Self> {
        let http =
            crate::http::client(&config.user_agent, config.timeout, config.proxy.as_deref())?;
        let candidates = instances
            .into_iter()
            .map(|url| Candidate {
                url,
                search_latency: None,
                uptime: None,
                version: None,
                score: 0.5,
            })
            .collect();
        // No directory timestamp: an explicit list is never refreshed from
        // searx.space, since replacing a caller's chosen instances with the
        // internet's would be a surprise.
        Ok(Self::from_candidates(config, http, candidates, None))
    }

    fn from_candidates(
        config: Config,
        http: Client,
        candidates: Vec<Candidate>,
        fetched_at: Option<Instant>,
    ) -> Self {
        let instances = candidates
            .into_iter()
            .map(|candidate| Instance::new(candidate.url, candidate.score))
            .collect();
        PublicSearxNg {
            http,
            scheduler: Arc::new(Scheduler::new(instances, config.policy)),
            config,
            replenish: Mutex::new(Replenish {
                directory_fetched_at: fetched_at,
                probed_at: None,
            }),
        }
    }

    /// The pool's current state. For logs, metrics, and tests.
    pub fn census(&self) -> Census {
        self.scheduler.census(Instant::now())
    }

    pub fn instances(&self) -> Vec<Instance> {
        self.scheduler.snapshot()
    }

    async fn run(&self, query: &Query) -> Result<Results> {
        let mut last_error: Option<Error> = None;

        for attempt in 0..self.config.max_attempts.max(1) {
            let lease = match self.scheduler.acquire(Instant::now()) {
                Some(lease) => lease,
                None => {
                    // Nothing verified is free: top up, then look once more.
                    // A replenish failure is not the query's failure if some
                    // instance is still usable, so it is only reported when
                    // there is nothing else to report.
                    if let Err(error) = self.replenish_now().await {
                        last_error.get_or_insert(error);
                    }
                    match self.wait_for_instance().await {
                        Some(lease) => lease,
                        None => break,
                    }
                }
            };

            match self.query_leased(&lease, query).await {
                Ok(results) => return Ok(results),
                Err(error) => {
                    tracing::debug!(
                        instance = %lease.name,
                        attempt,
                        %error,
                        "instance failed, moving to another"
                    );
                    // A refusal aimed at the query rather than at us — a
                    // malformed request, say — will be refused identically
                    // everywhere, so failing over would just spend other
                    // people's instances to learn the same thing.
                    let worth_another_instance = error.is_transient()
                        || error.disqualifies_instance()
                        || matches!(error, Error::Decode(_));
                    if !worth_another_instance {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            let census = self.census();
            Error::NoInstance {
                considered: census.total,
                verified: census.verified,
            }
        }))
    }

    /// Waits a bounded time for an instance to come free.
    ///
    /// Verified pools are small in practice — a single-digit count is the
    /// normal case, since most public instances do not serve JSON at all — and
    /// the in-flight cap that keeps us from bursting one of them also means a
    /// second concurrent query can arrive to find every instance busy. Failing
    /// that query outright would make the anti-bursting rule read as
    /// flakiness, so it waits briefly for a turn instead.
    ///
    /// Polling rather than a queue: the thing being waited on is often a
    /// cooldown expiring, which no one is going to signal.
    async fn wait_for_instance(&self) -> Option<Lease> {
        let deadline = Instant::now() + self.config.wait_for_instance;
        loop {
            if let Some(lease) = self.scheduler.acquire(Instant::now()) {
                return Some(lease);
            }
            // Nothing is coming free if there is nothing to come free.
            if self.census().verified == 0 || Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
        }
    }

    /// Runs one query against a leased instance and reports the outcome back
    /// to the scheduler. The lease is always released, on both paths.
    async fn query_leased(&self, lease: &Lease, query: &Query) -> Result<Results> {
        let engine = SearxNg::with_client(self.http.clone(), lease.url.clone());
        let started = Instant::now();
        let outcome = engine.search(query).await;
        let finished = Instant::now();

        match &outcome {
            Ok(_) => self.scheduler.release(
                lease,
                finished,
                Outcome::Ok {
                    latency: finished - started,
                },
            ),
            Err(error) => self.scheduler.release(lease, finished, Outcome::Err(error)),
        }
        outcome
    }

    /// Re-fetches the directory if it is stale and probes until the pool holds
    /// [`Config::target_verified`] verified instances or runs out of
    /// candidates.
    ///
    /// Safe to call at any time; concurrent callers queue on one lock and the
    /// later ones usually find the work already done.
    pub async fn replenish_now(&self) -> Result<()> {
        let mut state = self.replenish.lock().await;

        if self.census().verified >= self.config.target_verified {
            return Ok(());
        }

        let stale = state
            .directory_fetched_at
            .is_some_and(|at| at.elapsed() >= self.config.directory_ttl);
        if stale {
            match directory::fetch(&self.http, &self.config.directory_url).await {
                Ok(candidates) => {
                    let added = self.scheduler.merge(
                        candidates
                            .into_iter()
                            .map(|candidate| Instance::new(candidate.url, candidate.score)),
                    );
                    state.directory_fetched_at = Some(Instant::now());
                    tracing::debug!(added, "refreshed instance directory");
                }
                // A stale list is worth more than no list; the instances we
                // already know about have not stopped existing.
                Err(error) => tracing::warn!(%error, "directory refresh failed, keeping old list"),
            }
        }

        // Sweeps are rate-limited from our side too. Cooled-down instances
        // keep becoming probeable again, so an unbounded sweep-per-query would
        // be a steady drum on the whole network.
        if state
            .probed_at
            .is_some_and(|at| at.elapsed() < self.config.replenish_interval)
        {
            return self.verified_or_else();
        }
        state.probed_at = Some(Instant::now());

        self.probe(&mut state).await
    }

    /// `Ok` if the pool can still serve, [`Error::NoInstance`] if it cannot.
    fn verified_or_else(&self) -> Result<()> {
        let census = self.census();
        if census.verified == 0 {
            return Err(Error::NoInstance {
                considered: census.total,
                verified: 0,
            });
        }
        Ok(())
    }

    async fn probe(&self, _state: &mut Replenish) -> Result<()> {
        let probe_query = Query::new(self.config.probe_query.clone()).with_limit(1);
        let mut probed = 0usize;

        while self.census().verified < self.config.target_verified
            && probed < self.config.max_probes
        {
            let now = Instant::now();
            let batch: Vec<Lease> = (0..self.config.probe_batch.max(1))
                .filter_map(|_| self.scheduler.acquire_probe(now))
                .collect();
            if batch.is_empty() {
                break;
            }
            probed += batch.len();

            let attempts = batch
                .iter()
                .map(|lease| self.query_leased(lease, &probe_query));
            // Results are discarded: a probe answers "does this instance serve
            // JSON", and handing its hits to a caller who asked something else
            // would be a different question's answer.
            for (lease, outcome) in batch
                .iter()
                .zip(futures_util::future::join_all(attempts).await)
            {
                match outcome {
                    Ok(_) => tracing::debug!(instance = %lease.name, "instance verified"),
                    Err(error) => {
                        tracing::trace!(instance = %lease.name, %error, "probe failed")
                    }
                }
            }
        }

        self.verified_or_else()
    }
}

#[async_trait]
impl SearchEngine for PublicSearxNg {
    async fn search(&self, query: &Query) -> Result<Results> {
        self.run(query).await
    }

    fn provider(&self) -> &str {
        "searxng-public"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(urls: &[&str]) -> PublicSearxNg {
        PublicSearxNg::from_instances(
            Config::default(),
            urls.iter().map(|u| url::Url::parse(u).unwrap()).collect(),
        )
        .unwrap()
    }

    #[test]
    fn an_explicit_list_becomes_an_unverified_pool() {
        let engine = pool(&["https://a.example/", "https://b.example/"]);
        let census = engine.census();
        assert_eq!(
            (census.total, census.unverified, census.verified),
            (2, 2, 0)
        );
        assert_eq!(engine.provider(), "searxng-public");
    }

    #[tokio::test]
    async fn an_empty_pool_reports_no_instance_rather_than_hanging() {
        let engine = pool(&[]);
        let started = Instant::now();
        let error = engine.search(&Query::new("anything")).await.unwrap_err();
        assert!(matches!(
            error,
            Error::NoInstance {
                considered: 0,
                verified: 0
            }
        ));
        // The wait is for a busy instance to free up; with no instance at all
        // there is nothing to wait for and the caller must not be held.
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// The pool must not point real traffic at hosts it has never verified;
    /// unreachable ones are probed, fail, and leave the pool empty rather than
    /// producing a bogus answer.
    #[tokio::test]
    async fn unreachable_instances_never_serve_a_query() {
        let config = Config {
            timeout: Duration::from_millis(300),
            max_probes: 2,
            probe_batch: 2,
            ..Config::default()
        };
        // Reserved for documentation use; nothing answers here.
        let engine = PublicSearxNg::from_instances(
            config,
            vec![url::Url::parse("https://searx.invalid./").unwrap()],
        )
        .unwrap();

        let error = engine.search(&Query::new("anything")).await.unwrap_err();
        assert!(matches!(error, Error::NoInstance { .. }), "got {error}");
        assert_eq!(engine.census().verified, 0);
    }

    /// Networked. Run with `cargo test -p search -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "talks to searx.space and to public instances"]
    async fn discovers_probes_and_searches_the_live_network() {
        let engine = PublicSearxNg::discover(Config::default()).await.unwrap();
        let census = engine.census();
        println!("{census:?}");
        assert!(census.verified >= 1);

        let results = engine
            .search(&Query::new("rust ownership").with_limit(5))
            .await
            .unwrap();
        println!(
            "served by {:?}: {} hits",
            results.source,
            results.hits.len()
        );
        assert!(!results.hits.is_empty());

        // And the load spreads: several queries should not all land on one.
        let mut sources = std::collections::HashSet::new();
        for term in [
            "borrow checker",
            "tokio runtime",
            "serde derive",
            "axum router",
        ] {
            if let Ok(results) = engine.search(&Query::new(term)).await {
                sources.insert(results.source.clone());
            }
        }
        println!("sources: {sources:?}");
        assert!(sources.len() > 1 || census.verified == 1);
    }
}
