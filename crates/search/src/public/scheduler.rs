//! Choosing which instance answers the next query.
//!
//! This is the part the public provider exists for, so it is written as a
//! plain, synchronous, deterministic unit with no I/O in it: everything it
//! needs about time arrives as an [`Instant`] argument. Distribution is a
//! property you can only check by running many selections, and a component
//! that had to make HTTP calls to be exercised would not get checked.
//!
//! ## Why not "use the best instance"
//!
//! Because that is the failure mode. Score-maximising selection returns the
//! same winner every time, which turns the healthiest instance into the one we
//! burst until its limiter cuts us off — and then does the same to the next.
//! Public instances are volunteers' servers; leaning on one is both worse for
//! us and rude.
//!
//! So selection is two stages, and only the first one looks at quality:
//!
//! 1. **Eligibility.** Verified as serving JSON, out of cooldown, and not
//!    already busy. The in-flight cap is the part that actually prevents
//!    bursting — without it, concurrent queries all pick the same instance
//!    before any of them has finished.
//! 2. **Rotation.** Take the top [`Policy::tier`] eligible instances by score,
//!    then pick the **least recently used** of them. Score decides who is in
//!    the room; recency decides whose turn it is. Weighted-random would
//!    distribute too, but LRU is deterministic, which means it is testable.
//!
//! `last_used` is stamped at *acquire*, not at release, so two selections
//! racing each other cannot both see the same instance as least recent.
//!
//! ## Two kinds of bad news
//!
//! A rate limit is a request to come back later: the instance goes into an
//! exponential cooldown (honouring `Retry-After` when it sends one) and stays
//! in the pool. An instance that answers a JSON request with HTML is
//! disqualified permanently — no configuration of ours will change its
//! `settings.yml`, and leaving it in to be retried forever is how a pool
//! starves. Everything else gets a cooldown and is disqualified only after
//! repeated failures.
//!
//! **A failed probe is not a permanent verdict**, and this is the sharpest
//! edge in the module. Measured against the live network, 53 of 75 instances
//! answered a first contact with the limiter's `429` — so a pool that retires
//! an instance on its first failed probe retires most of the network before it
//! has learned anything, and is left rotating across the one host that
//! happened to answer. Probes get a shorter rope than verified instances
//! ([`Policy::probe_max_consecutive_failures`]), not a single strike.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use url::Url;

use crate::error::Error;

/// What the scheduler knows about an instance's ability to serve us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Never successfully queried. Eligible for probing, not for real work.
    Unverified,
    /// Has answered a JSON query.
    Verified,
    /// Cannot serve us, ever. Never selected again.
    Disqualified,
}

/// Selection and cooldown constants.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Concurrent queries allowed against one instance.
    ///
    /// One by default: with several verified instances available there is no
    /// reason to double up on any of them, and this is what stops a burst of
    /// concurrent queries from landing on a single host.
    pub max_in_flight: u32,
    /// How many of the best eligible instances rotation draws from.
    ///
    /// Small enough that quality still matters, large enough that no single
    /// instance sees a big share of the traffic.
    pub tier: usize,
    /// First cooldown after a rate limit; doubles per consecutive failure.
    pub rate_limit_cooldown: Duration,
    /// First cooldown after a transport or server error.
    pub failure_cooldown: Duration,
    /// Ceiling for either cooldown.
    pub max_cooldown: Duration,
    /// Consecutive *failures* before a verified instance is dropped for good.
    ///
    /// Throttles are not failures and never count here — see
    /// [`Instance::consecutive_throttles`].
    pub max_consecutive_failures: u32,
    /// Consecutive bad answers of any kind, throttles included, before an
    /// instance that has never worked is dropped.
    ///
    /// Lower, and it counts throttles, because a probe spends someone else's
    /// search to learn nothing most of the time — but not one, because the
    /// usual first answer is a rate limit rather than a refusal.
    pub probe_max_consecutive_failures: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            max_in_flight: 1,
            tier: 5,
            rate_limit_cooldown: Duration::from_secs(60),
            failure_cooldown: Duration::from_secs(15),
            max_cooldown: Duration::from_secs(30 * 60),
            max_consecutive_failures: 5,
            probe_max_consecutive_failures: 3,
        }
    }
}

/// The scheduler's record of one instance.
#[derive(Clone, Debug)]
pub struct Instance {
    pub url: Url,
    pub name: String,
    /// The directory's opinion, `0.0..=1.0`. Fixed at construction.
    pub base_score: f32,
    pub health: Health,
    pub in_flight: u32,
    pub last_used: Option<Instant>,
    pub cooldown_until: Option<Instant>,
    /// Consecutive real failures — transport, `5xx`, junk bodies. Drives
    /// retirement.
    pub consecutive_failures: u32,
    /// Consecutive throttles. Drives backoff only: being told to slow down is
    /// not evidence against an instance that has already worked.
    pub consecutive_throttles: u32,
    pub successes: u64,
    pub failures: u64,
    /// Exponentially weighted mean seconds per successful query.
    pub mean_latency: Option<f32>,
}

impl Instance {
    pub fn new(url: Url, base_score: f32) -> Self {
        let name = url.host_str().unwrap_or("instance").to_owned();
        Instance {
            url,
            name,
            base_score,
            health: Health::Unverified,
            in_flight: 0,
            last_used: None,
            cooldown_until: None,
            consecutive_failures: 0,
            consecutive_throttles: 0,
            successes: 0,
            failures: 0,
            mean_latency: None,
        }
    }

    fn available(&self, now: Instant, policy: &Policy) -> bool {
        self.health != Health::Disqualified
            && self.in_flight < policy.max_in_flight
            && self.cooldown_until.is_none_or(|until| until <= now)
    }

    /// Directory opinion, corrected by what we have actually observed.
    ///
    /// Observation outweighs the directory once there is any: the file was
    /// written by someone else's crawler from someone else's IP, and our own
    /// success rate against the instance is the measurement that matters.
    fn score(&self) -> f32 {
        let attempts = self.successes + self.failures;
        if attempts == 0 {
            return self.base_score;
        }
        let observed = self.successes as f32 / attempts as f32;
        let speed = self
            .mean_latency
            .map_or(0.5, |seconds| (1.0 / (1.0 + seconds.max(0.0))).min(1.0));
        // Confidence in the observation grows with the sample; five attempts
        // is enough to stop deferring to the directory.
        let weight = (attempts as f32 / 5.0).min(1.0);
        let measured = 0.75 * observed + 0.25 * speed;
        (1.0 - weight) * self.base_score + weight * measured
    }
}

/// A claim on one instance, to be handed back to [`Scheduler::release`].
///
/// Holds an index rather than a borrow so the scheduler's lock is not held
/// across the query — the whole point is that many queries are in flight at
/// once.
///
/// The index stays valid because the instance list only ever grows:
/// [`Scheduler::merge`] pushes, and disqualification marks rather than
/// removes. Compacting that list would silently reattribute outcomes to the
/// wrong instance, so it must not be compacted without giving instances ids.
#[derive(Clone, Debug)]
pub struct Lease {
    index: usize,
    pub url: Url,
    pub name: String,
    /// Whether this selection was for real work or for a JSON probe.
    pub probing: bool,
}

/// How a leased query ended.
pub enum Outcome<'a> {
    Ok { latency: Duration },
    Err(&'a Error),
}

/// The instance pool and its selection policy.
///
/// In memory only. Faber serves many users from one process; instance health
/// observed while serving one user is a property of the network at this
/// moment, not something to persist or share as configuration.
#[derive(Debug)]
pub struct Scheduler {
    instances: Mutex<Vec<Instance>>,
    policy: Policy,
}

impl Scheduler {
    pub fn new(instances: Vec<Instance>, policy: Policy) -> Self {
        Scheduler {
            instances: Mutex::new(instances),
            policy,
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Claims a verified instance for a real query.
    ///
    /// `None` means every verified instance is busy, cooling down, or gone —
    /// which is the caller's cue to probe for more, not to give up.
    pub fn acquire(&self, now: Instant) -> Option<Lease> {
        self.select(now, Health::Verified)
    }

    /// Claims an unverified instance to probe.
    ///
    /// Separate from [`acquire`](Self::acquire) because probing is speculative:
    /// most public instances do not serve JSON at all, so a probe is expected
    /// to fail and must never be handed a user's query.
    pub fn acquire_probe(&self, now: Instant) -> Option<Lease> {
        self.select(now, Health::Unverified)
    }

    fn select(&self, now: Instant, wanted: Health) -> Option<Lease> {
        let mut instances = self.instances.lock().expect("scheduler lock");

        let mut eligible: Vec<usize> = instances
            .iter()
            .enumerate()
            .filter(|(_, instance)| {
                instance.health == wanted && instance.available(now, &self.policy)
            })
            .map(|(index, _)| index)
            .collect();
        if eligible.is_empty() {
            return None;
        }

        // Stage one: quality. Best first, ties broken by position so the
        // choice never depends on hash or thread ordering.
        eligible.sort_by(|&a, &b| {
            instances[b]
                .score()
                .partial_cmp(&instances[a].score())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        eligible.truncate(self.policy.tier.max(1));

        // Stage two: rotation. Never used wins outright; otherwise the
        // longest-idle instance takes the turn.
        let chosen = *eligible
            .iter()
            .min_by_key(|&&index| instances[index].last_used)
            .expect("non-empty after the emptiness check");

        let instance = &mut instances[chosen];
        instance.in_flight += 1;
        instance.last_used = Some(now);
        Some(Lease {
            index: chosen,
            url: instance.url.clone(),
            name: instance.name.clone(),
            probing: wanted == Health::Unverified,
        })
    }

    /// Returns a lease and records what happened.
    pub fn release(&self, lease: &Lease, now: Instant, outcome: Outcome<'_>) {
        let mut instances = self.instances.lock().expect("scheduler lock");
        let policy = self.policy;
        let Some(instance) = instances.get_mut(lease.index) else {
            return;
        };
        instance.in_flight = instance.in_flight.saturating_sub(1);

        match outcome {
            Outcome::Ok { latency } => {
                instance.health = Health::Verified;
                instance.successes += 1;
                instance.consecutive_failures = 0;
                instance.consecutive_throttles = 0;
                instance.cooldown_until = None;
                let seconds = latency.as_secs_f32();
                instance.mean_latency = Some(match instance.mean_latency {
                    Some(mean) => 0.7 * mean + 0.3 * seconds,
                    None => seconds,
                });
            }
            Outcome::Err(error) => {
                instance.failures += 1;

                if error.disqualifies_instance() {
                    instance.health = Health::Disqualified;
                    instance.cooldown_until = None;
                    return;
                }

                // Throttles and failures are counted apart. A `429` is the
                // instance working correctly and asking us to slow down; it
                // should lengthen the backoff and nothing else. Spending
                // retirement budget on it would retire a *proven* instance for
                // being popular — and with a pool this thin, every queued
                // query returns to the same host the moment its cooldown
                // lapses, so the count would march to the limit under nothing
                // worse than sustained use.
                let throttled = error.is_throttle();
                let base = if throttled {
                    instance.consecutive_throttles += 1;
                    policy.rate_limit_cooldown
                } else {
                    instance.consecutive_failures += 1;
                    policy.failure_cooldown
                };
                let escalation = if throttled {
                    instance.consecutive_throttles
                } else {
                    instance.consecutive_failures
                };
                // The instance's own `Retry-After` beats our guess whenever it
                // asks for longer; asking for less is not binding on us.
                let asked = error.retry_after().unwrap_or(Duration::ZERO);
                let backoff = backoff(base, escalation, policy.max_cooldown);
                instance.cooldown_until = Some(now + backoff.max(asked).min(policy.max_cooldown));

                // An instance that keeps *failing* is spent. Probes get a
                // shorter rope, and theirs counts throttles too: an unverified
                // instance that has turned away three of our probes has spent
                // three of somebody's searches teaching us nothing, which is a
                // cost worth bounding. A verified one has already proved it
                // works, so only real failures count against it.
                let retired = if instance.health == Health::Unverified {
                    instance.consecutive_failures + instance.consecutive_throttles
                        >= policy.probe_max_consecutive_failures
                } else {
                    instance.consecutive_failures >= policy.max_consecutive_failures
                };
                if retired {
                    instance.health = Health::Disqualified;
                }
            }
        }
    }

    /// Adds instances the pool has not seen, ignoring ones it has.
    ///
    /// Called on a directory refresh. Existing records keep their observed
    /// health: the directory has nothing to say about JSON support, so letting
    /// a refresh reset a disqualification would resurrect known-dead instances
    /// every few hours.
    pub fn merge(&self, candidates: impl IntoIterator<Item = Instance>) -> usize {
        let mut instances = self.instances.lock().expect("scheduler lock");
        let mut added = 0;
        for candidate in candidates {
            if instances.iter().any(|known| known.url == candidate.url) {
                continue;
            }
            instances.push(candidate);
            added += 1;
        }
        added
    }

    /// Counts, for error reporting and for deciding whether to probe.
    pub fn census(&self, now: Instant) -> Census {
        let instances = self.instances.lock().expect("scheduler lock");
        let mut census = Census {
            total: instances.len(),
            ..Census::default()
        };
        for instance in instances.iter() {
            match instance.health {
                Health::Verified => {
                    census.verified += 1;
                    if instance.available(now, &self.policy) {
                        census.available += 1;
                    }
                }
                Health::Unverified => {
                    census.unverified += 1;
                    if instance.available(now, &self.policy) {
                        census.probeable += 1;
                    }
                }
                Health::Disqualified => census.disqualified += 1,
            }
        }
        census
    }

    /// A copy of the pool's state, for logging and tests.
    pub fn snapshot(&self) -> Vec<Instance> {
        self.instances.lock().expect("scheduler lock").clone()
    }
}

/// What the pool currently consists of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    pub total: usize,
    pub verified: usize,
    /// Verified and selectable right now.
    pub available: usize,
    pub unverified: usize,
    /// Unverified and probeable right now.
    pub probeable: usize,
    pub disqualified: usize,
}

fn backoff(base: Duration, consecutive_failures: u32, max: Duration) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(10);
    base.saturating_mul(1u32 << shift).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(count: usize, policy: Policy) -> Scheduler {
        let instances = (0..count)
            .map(|i| {
                let url = Url::parse(&format!("https://instance{i}.example/")).unwrap();
                let mut instance = Instance::new(url, 1.0 - i as f32 * 0.05);
                instance.health = Health::Verified;
                instance
            })
            .collect();
        Scheduler::new(instances, policy)
    }

    fn run(scheduler: &Scheduler, queries: usize, start: Instant) -> Vec<String> {
        let mut served = Vec::new();
        for step in 0..queries {
            let now = start + Duration::from_millis(step as u64 * 10);
            let lease = scheduler.acquire(now).expect("an instance should be free");
            served.push(lease.name.clone());
            scheduler.release(
                &lease,
                now,
                Outcome::Ok {
                    latency: Duration::from_millis(500),
                },
            );
        }
        served
    }

    /// The requirement: spread the load, do not burst the last known-good
    /// instance. Sequential queries against a healthy pool must rotate.
    #[test]
    fn distributes_across_the_tier_instead_of_bursting_one() {
        let scheduler = pool(5, Policy::default());
        let served = run(&scheduler, 100, Instant::now());

        let mut counts = std::collections::HashMap::new();
        for name in &served {
            *counts.entry(name.clone()).or_insert(0usize) += 1;
        }
        assert_eq!(counts.len(), 5, "every instance should take a turn");
        for (name, count) in &counts {
            assert!(
                *count <= 100 / 3,
                "{name} took {count} of 100 queries; the point is not to burst one"
            );
        }
        // No instance twice in a row while others are idle.
        assert!(served.windows(2).all(|pair| pair[0] != pair[1]));
    }

    /// The tier bounds how far down the quality list rotation reaches, so a
    /// big pool still concentrates on good instances — just not on one.
    #[test]
    fn rotation_stays_inside_the_tier() {
        let scheduler = pool(20, Policy::default());
        let served = run(&scheduler, 60, Instant::now());
        let distinct: std::collections::HashSet<_> = served.iter().collect();
        assert_eq!(distinct.len(), Policy::default().tier);
    }

    #[test]
    fn concurrent_queries_do_not_pile_onto_one_instance() {
        let scheduler = pool(3, Policy::default());
        let now = Instant::now();
        let leases: Vec<_> = (0..3).map(|_| scheduler.acquire(now).unwrap()).collect();
        let names: std::collections::HashSet<_> = leases.iter().map(|l| &l.name).collect();
        assert_eq!(names.len(), 3, "in-flight cap should force different hosts");
        // A fourth has nowhere to go until one of them finishes.
        assert!(scheduler.acquire(now).is_none());
        scheduler.release(
            &leases[0],
            now,
            Outcome::Ok {
                latency: Duration::from_millis(100),
            },
        );
        assert!(scheduler.acquire(now).is_some());
    }

    #[test]
    fn a_rate_limited_instance_is_skipped_until_its_cooldown_expires() {
        let policy = Policy {
            tier: 2,
            ..Policy::default()
        };
        let scheduler = pool(2, policy);
        let start = Instant::now();

        let lease = scheduler.acquire(start).unwrap();
        let limited = lease.name.clone();
        scheduler.release(
            &lease,
            start,
            Outcome::Err(&Error::RateLimited { retry_after: None }),
        );

        // For the whole cooldown, the other instance takes every query.
        for step in 1..30u64 {
            let now = start + Duration::from_secs(step * 2);
            let lease = scheduler.acquire(now).unwrap();
            assert_ne!(lease.name, limited, "cooled-down instance was selected");
            scheduler.release(
                &lease,
                now,
                Outcome::Ok {
                    latency: Duration::from_millis(200),
                },
            );
        }

        // And once it expires it comes back into rotation.
        let later = start + policy.rate_limit_cooldown + Duration::from_secs(1);
        let names: std::collections::HashSet<_> = (0..4)
            .map(|i| {
                let now = later + Duration::from_millis(i * 10);
                let lease = scheduler.acquire(now).unwrap();
                let name = lease.name.clone();
                scheduler.release(
                    &lease,
                    now,
                    Outcome::Ok {
                        latency: Duration::from_millis(200),
                    },
                );
                name
            })
            .collect();
        assert!(names.contains(&limited));
    }

    #[test]
    fn retry_after_extends_the_cooldown_but_never_shortens_it() {
        let scheduler = pool(1, Policy::default());
        let start = Instant::now();

        let lease = scheduler.acquire(start).unwrap();
        scheduler.release(
            &lease,
            start,
            Outcome::Err(&Error::RateLimited {
                retry_after: Some(Duration::from_secs(600)),
            }),
        );
        assert!(
            scheduler
                .acquire(start + Duration::from_secs(300))
                .is_none()
        );
        assert!(
            scheduler
                .acquire(start + Duration::from_secs(601))
                .is_some()
        );

        let scheduler = pool(1, Policy::default());
        let lease = scheduler.acquire(start).unwrap();
        scheduler.release(
            &lease,
            start,
            Outcome::Err(&Error::RateLimited {
                retry_after: Some(Duration::from_secs(1)),
            }),
        );
        assert!(
            scheduler.acquire(start + Duration::from_secs(2)).is_none(),
            "an instance asking for 1s still gets our full first cooldown"
        );
    }

    /// A verified instance may be told to slow down as often as it likes
    /// without being retired for it. With a pool as thin as the live network
    /// actually provides, every queued query returns to the same host as soon
    /// as its cooldown lapses, so counting throttles toward retirement would
    /// kill the pool under nothing worse than steady use.
    #[test]
    fn throttling_never_retires_a_verified_instance() {
        let policy = Policy::default();
        let scheduler = pool(1, policy);
        let mut now = Instant::now();

        for _ in 0..policy.max_consecutive_failures * 3 {
            let lease = scheduler
                .acquire(now)
                .expect("a throttled instance comes back after its cooldown");
            scheduler.release(
                &lease,
                now,
                Outcome::Err(&Error::RateLimited { retry_after: None }),
            );
            now = scheduler.snapshot()[0].cooldown_until.unwrap() + Duration::from_secs(1);
        }

        let instance = &scheduler.snapshot()[0];
        assert_eq!(instance.health, Health::Verified);
        assert_eq!(instance.consecutive_failures, 0, "a 429 is not a failure");
        assert!(scheduler.acquire(now).is_some());
    }

    #[test]
    fn cooldown_grows_with_consecutive_failures_and_is_capped() {
        let policy = Policy::default();
        let scheduler = pool(1, policy);
        let mut now = Instant::now();
        let mut previous = Duration::ZERO;

        for _ in 0..4 {
            let lease = scheduler.acquire(now).unwrap();
            scheduler.release(
                &lease,
                now,
                Outcome::Err(&Error::RateLimited { retry_after: None }),
            );
            let until = scheduler.snapshot()[0].cooldown_until.unwrap();
            let waited = until - now;
            assert!(waited > previous, "backoff should grow");
            assert!(waited <= policy.max_cooldown);
            previous = waited;
            now = until + Duration::from_secs(1);
        }
    }

    #[test]
    fn a_success_clears_the_backoff() {
        let scheduler = pool(1, Policy::default());
        let start = Instant::now();
        let lease = scheduler.acquire(start).unwrap();
        scheduler.release(
            &lease,
            start,
            Outcome::Err(&Error::RateLimited { retry_after: None }),
        );

        let now = start + Duration::from_secs(61);
        let lease = scheduler.acquire(now).unwrap();
        scheduler.release(
            &lease,
            now,
            Outcome::Ok {
                latency: Duration::from_millis(300),
            },
        );
        let instance = &scheduler.snapshot()[0];
        assert_eq!(instance.consecutive_failures, 0);
        assert!(instance.cooldown_until.is_none());
    }

    #[test]
    fn an_instance_without_the_json_api_is_gone_for_good() {
        let scheduler = pool(2, Policy::default());
        let start = Instant::now();
        let lease = scheduler.acquire(start).unwrap();
        let dead = lease.name.clone();
        scheduler.release(
            &lease,
            start,
            Outcome::Err(&Error::NoJsonApi {
                reason: "html".into(),
            }),
        );

        for step in 1..50u64 {
            let now = start + Duration::from_secs(step * 600);
            let lease = scheduler.acquire(now).unwrap();
            assert_ne!(lease.name, dead);
            scheduler.release(
                &lease,
                now,
                Outcome::Ok {
                    latency: Duration::from_millis(200),
                },
            );
        }
        assert_eq!(scheduler.census(start).disqualified, 1);
    }

    #[test]
    fn repeated_failures_retire_an_instance() {
        let policy = Policy {
            max_consecutive_failures: 3,
            ..Policy::default()
        };
        let scheduler = pool(2, policy);
        let mut now = Instant::now();

        let target = scheduler.snapshot()[0].name.clone();
        for _ in 0..3 {
            // Drive the failure onto one specific instance by leasing both and
            // failing only the one under test.
            let a = scheduler.acquire(now).unwrap();
            let b = scheduler.acquire(now).unwrap();
            let (failing, other) = if a.name == target { (a, b) } else { (b, a) };
            scheduler.release(
                &failing,
                now,
                Outcome::Err(&Error::Api {
                    status: 502,
                    message: "bad gateway".into(),
                }),
            );
            scheduler.release(
                &other,
                now,
                Outcome::Ok {
                    latency: Duration::from_millis(100),
                },
            );
            now += Duration::from_secs(60 * 60);
        }

        assert_eq!(
            scheduler
                .snapshot()
                .iter()
                .find(|i| i.name == target)
                .unwrap()
                .health,
            Health::Disqualified
        );
    }

    #[test]
    fn probes_never_serve_real_queries_and_promote_on_success() {
        let instances = vec![
            Instance::new(Url::parse("https://a.example/").unwrap(), 0.9),
            Instance::new(Url::parse("https://b.example/").unwrap(), 0.8),
        ];
        let scheduler = Scheduler::new(instances, Policy::default());
        let now = Instant::now();

        assert!(scheduler.acquire(now).is_none(), "nothing is verified yet");

        let probe = scheduler.acquire_probe(now).unwrap();
        assert!(probe.probing);
        scheduler.release(
            &probe,
            now,
            Outcome::Err(&Error::NoJsonApi {
                reason: "html".into(),
            }),
        );

        let probe = scheduler.acquire_probe(now).unwrap();
        scheduler.release(
            &probe,
            now,
            Outcome::Ok {
                latency: Duration::from_millis(400),
            },
        );

        let census = scheduler.census(now);
        assert_eq!((census.verified, census.disqualified), (1, 1));
        assert_eq!(scheduler.acquire(now).unwrap().name, probe.name);
    }

    /// The measured failure mode: a first probe usually meets the bot
    /// limiter. Retiring the instance for it would empty the pool before it
    /// filled, so a rate-limited probe cools down and is tried again.
    #[test]
    fn a_rate_limited_probe_comes_back_after_its_cooldown() {
        let policy = Policy::default();
        let instances = vec![Instance::new(
            Url::parse("https://a.example/").unwrap(),
            0.9,
        )];
        let scheduler = Scheduler::new(instances, policy);
        let now = Instant::now();

        let probe = scheduler.acquire_probe(now).unwrap();
        scheduler.release(
            &probe,
            now,
            Outcome::Err(&Error::RateLimited { retry_after: None }),
        );

        assert!(scheduler.acquire_probe(now).is_none(), "still cooling down");
        assert_eq!(scheduler.census(now).unverified, 1, "not retired");

        let later = now + policy.rate_limit_cooldown + Duration::from_secs(1);
        let probe = scheduler.acquire_probe(later).unwrap();
        scheduler.release(
            &probe,
            later,
            Outcome::Ok {
                latency: Duration::from_millis(300),
            },
        );
        assert_eq!(scheduler.census(later).verified, 1);
    }

    /// Patience is finite, though: an instance that never answers stops being
    /// probed rather than being retried forever.
    #[test]
    fn a_probe_that_keeps_failing_is_eventually_retired() {
        let policy = Policy::default();
        let instances = vec![Instance::new(
            Url::parse("https://a.example/").unwrap(),
            0.9,
        )];
        let scheduler = Scheduler::new(instances, policy);
        let mut now = Instant::now();

        for _ in 0..policy.probe_max_consecutive_failures {
            let probe = scheduler
                .acquire_probe(now)
                .expect("probeable until the limit");
            scheduler.release(
                &probe,
                now,
                Outcome::Err(&Error::RateLimited { retry_after: None }),
            );
            now += policy.max_cooldown + Duration::from_secs(1);
        }
        assert_eq!(scheduler.census(now).disqualified, 1);
    }

    /// A `403` is the limiter as often as it is a missing JSON API, and
    /// nothing distinguishes them — so it cools down rather than convicting.
    #[test]
    fn a_forbidden_response_cools_down_rather_than_retiring() {
        let scheduler = pool(1, Policy::default());
        let now = Instant::now();
        let lease = scheduler.acquire(now).unwrap();
        scheduler.release(
            &lease,
            now,
            Outcome::Err(&Error::Api {
                status: 403,
                message: "forbidden".into(),
            }),
        );

        let instance = &scheduler.snapshot()[0];
        assert_eq!(instance.health, Health::Verified);
        // And it got the throttle cooldown, not the shorter failure one.
        let waited = instance.cooldown_until.unwrap() - now;
        assert_eq!(waited, Policy::default().rate_limit_cooldown);
    }

    #[test]
    fn observed_failure_outranks_the_directorys_optimism() {
        let mut good_on_paper = Instance::new(Url::parse("https://paper.example/").unwrap(), 1.0);
        good_on_paper.health = Health::Verified;
        good_on_paper.successes = 1;
        good_on_paper.failures = 9;
        let mut proven = Instance::new(Url::parse("https://proven.example/").unwrap(), 0.4);
        proven.health = Health::Verified;
        proven.successes = 20;
        proven.mean_latency = Some(0.5);

        let scheduler = Scheduler::new(
            vec![good_on_paper, proven],
            Policy {
                tier: 1,
                ..Policy::default()
            },
        );
        let now = Instant::now();
        assert_eq!(scheduler.acquire(now).unwrap().name, "proven.example");
    }

    #[test]
    fn merge_adds_new_instances_without_resurrecting_dead_ones() {
        let scheduler = pool(1, Policy::default());
        let now = Instant::now();
        let lease = scheduler.acquire(now).unwrap();
        scheduler.release(
            &lease,
            now,
            Outcome::Err(&Error::NoJsonApi {
                reason: "html".into(),
            }),
        );

        let refreshed = vec![
            Instance::new(Url::parse("https://instance0.example/").unwrap(), 1.0),
            Instance::new(Url::parse("https://newcomer.example/").unwrap(), 0.7),
        ];
        assert_eq!(scheduler.merge(refreshed), 1);
        let census = scheduler.census(now);
        assert_eq!((census.total, census.disqualified), (2, 1));
    }
}
