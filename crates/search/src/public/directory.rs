//! The `searx.space` instance directory.
//!
//! `https://searx.space/data/instances.json` is a third-party file, regenerated
//! roughly daily by somebody else's crawler, and its shape has changed before.
//! Everything here is therefore tolerant: every field is optional, unknown
//! fields are ignored, and an instance whose record cannot be understood is
//! skipped rather than failing the whole fetch. A directory that parses to
//! *fewer* instances is a degraded pool; a directory that fails to parse is no
//! pool at all.
//!
//! What the directory can and cannot tell us matters for the design. It
//! reports reachability, TLS grade, response timing, and uptime — all useful
//! for ranking. It says **nothing** about whether the JSON API is enabled, and
//! there is no field that predicts it, so support has to be probed. See
//! [`super::pool`].

use std::collections::HashMap;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::error::{Error, Result};

pub const DIRECTORY_URL: &str = "https://searx.space/data/instances.json";

/// One instance worth trying, with the directory's opinion of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub url: Url,
    /// Median wall-clock seconds for a search, as measured by searx.space.
    pub search_latency: Option<f32>,
    /// Percentage over the last week.
    pub uptime: Option<f32>,
    /// The SearXNG version string the instance reports, when it does.
    pub version: Option<String>,
    /// Ranking score in `0.0..=1.0`, derived from the fields above.
    ///
    /// A starting opinion only. Once an instance has actually served us,
    /// observed behaviour is what the scheduler ranks on.
    pub score: f32,
}

/// Fetches and filters the directory.
///
/// Filtering is deliberately coarse — reachable, healthy, not Tor — because
/// the expensive judgement (does it serve JSON?) cannot be made from this file
/// and the cheap one should not pre-empt it.
pub async fn fetch(http: &Client, url: &str) -> Result<Vec<Candidate>> {
    let response = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| Error::Directory(format!("fetching {url}: {error}")))?;

    let status = response.status();
    if !status.is_success() {
        return Err(Error::Directory(format!("{url} answered {status}")));
    }

    let body = response
        .text()
        .await
        .map_err(|error| Error::Directory(format!("reading {url}: {error}")))?;
    parse(&body)
}

pub(crate) fn parse(body: &str) -> Result<Vec<Candidate>> {
    let file: Directory = serde_json::from_str(body)
        .map_err(|error| Error::Directory(format!("decoding instances.json: {error}")))?;

    let mut candidates: Vec<Candidate> = file
        .instances
        .into_iter()
        .filter_map(|(url, record)| record.into_candidate(&url))
        .collect();
    // Best first, so a caller that probes only a prefix probes the good ones.
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.url.as_str().cmp(b.url.as_str()))
    });

    if candidates.is_empty() {
        return Err(Error::Directory(
            "directory listed no reachable instances".into(),
        ));
    }
    Ok(candidates)
}

#[derive(Debug, Default, Deserialize)]
struct Directory {
    #[serde(default)]
    instances: HashMap<String, Record>,
}

#[derive(Debug, Default, Deserialize)]
struct Record {
    /// `"normal"` or `"tor"`. Tor instances need an onion-capable client this
    /// crate does not build, so they are dropped.
    #[serde(default)]
    network_type: Option<String>,
    /// `null` when healthy, a string like `"HTTP status code 502"` otherwise.
    /// Typed as `Value` because "the error field is a string" is exactly the
    /// kind of assumption this file breaks.
    #[serde(default)]
    error: serde_json::Value,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    http: Option<Http>,
    #[serde(default)]
    timing: Option<Timing>,
    #[serde(default)]
    uptime: Option<Uptime>,
}

#[derive(Debug, Default, Deserialize)]
struct Http {
    #[serde(default)]
    status_code: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
struct Timing {
    #[serde(default)]
    search: Option<Phase>,
}

#[derive(Debug, Default, Deserialize)]
struct Phase {
    #[serde(default)]
    success_percentage: Option<f32>,
    #[serde(default)]
    all: Option<Stat>,
}

#[derive(Debug, Default, Deserialize)]
struct Stat {
    #[serde(default)]
    median: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
struct Uptime {
    #[serde(rename = "uptimeWeek", default)]
    week: Option<f32>,
}

impl Record {
    fn into_candidate(self, url: &str) -> Option<Candidate> {
        if !self.error.is_null() {
            return None;
        }
        if self.network_type.as_deref().unwrap_or("normal") != "normal" {
            return None;
        }
        if self.http.as_ref().and_then(|http| http.status_code) != Some(200) {
            return None;
        }

        let url = Url::parse(url).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }

        let search = self.timing.and_then(|timing| timing.search);
        // An instance whose search probe never succeeded is not a candidate,
        // whatever its landing page says.
        let search_success = search.as_ref().and_then(|phase| phase.success_percentage);
        if search_success.is_some_and(|percentage| percentage <= 0.0) {
            return None;
        }
        let search_latency = search
            .and_then(|phase| phase.all)
            .and_then(|all| all.median);
        let uptime = self.uptime.and_then(|uptime| uptime.week);

        Some(Candidate {
            score: score(search_latency, uptime, search_success),
            url,
            search_latency,
            uptime,
            version: self.version,
        })
    }
}

/// Blends uptime, search success rate, and latency into `0.0..=1.0`.
///
/// Uptime and success rate dominate; latency is a tie-breaker. A missing
/// measurement scores as mediocre rather than as zero — the directory not
/// having probed something recently is not evidence against the instance, and
/// scoring it zero would bury newly listed instances forever.
fn score(latency: Option<f32>, uptime: Option<f32>, success: Option<f32>) -> f32 {
    let uptime = uptime.unwrap_or(90.0) / 100.0;
    let success = success.unwrap_or(80.0) / 100.0;
    // 0.5s → ~1.0, 2s → 0.5, 5s → 0.2.
    let speed = latency.map_or(0.5, |seconds| (1.0 / (1.0 + seconds.max(0.0))).min(1.0));
    (0.45 * uptime + 0.35 * success + 0.20 * speed).clamp(0.0, 1.0)
}

/// How long a fetched directory stays good.
///
/// The file regenerates on the order of a day; re-fetching per query would
/// hammer searx.space to learn nothing new.
pub const DEFAULT_DIRECTORY_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes taken from the live file: a healthy instance, one with an
    /// error, a Tor instance, one whose search probe never succeeds, and one
    /// carrying fields this crate has never seen.
    const SAMPLE: &str = r#"{
      "metadata": {"timestamp": 1786696301},
      "instances": {
        "https://good.example/": {
          "network_type": "normal",
          "error": null,
          "version": "2026.8.14+094c33d40",
          "http": {"status_code": 200, "grade": "A+"},
          "timing": {"search": {"success_percentage": 100.0, "all": {"median": 1.4}}},
          "uptime": {"uptimeDay": 100.0, "uptimeWeek": 99.9},
          "engines": {"bing": {}}
        },
        "https://slow.example/": {
          "network_type": "normal",
          "error": null,
          "http": {"status_code": 200},
          "timing": {"search": {"success_percentage": 60.0, "all": {"median": 6.0}}},
          "uptime": {"uptimeWeek": 80.0}
        },
        "https://broken.example/": {
          "network_type": "normal",
          "error": "HTTP status code 502",
          "http": {"status_code": 502}
        },
        "http://onion.example/": {
          "network_type": "tor",
          "error": null,
          "http": {"status_code": 200}
        },
        "https://dead-search.example/": {
          "network_type": "normal",
          "error": null,
          "http": {"status_code": 200},
          "timing": {"search": {"success_percentage": 0.0, "all": null}}
        },
        "https://future.example/": {
          "network_type": "normal",
          "error": null,
          "http": {"status_code": 200},
          "quantum_readiness": {"level": 11}
        }
      }
    }"#;

    #[test]
    fn keeps_only_usable_instances() {
        let candidates = parse(SAMPLE).unwrap();
        let hosts: Vec<_> = candidates
            .iter()
            .map(|c| c.url.host_str().unwrap().to_owned())
            .collect();
        assert!(hosts.contains(&"good.example".to_owned()));
        assert!(hosts.contains(&"slow.example".to_owned()));
        // Unknown fields must not disqualify an instance.
        assert!(hosts.contains(&"future.example".to_owned()));
        assert!(!hosts.contains(&"broken.example".to_owned()));
        assert!(!hosts.contains(&"onion.example".to_owned()));
        assert!(!hosts.contains(&"dead-search.example".to_owned()));
    }

    #[test]
    fn sorts_best_first() {
        let candidates = parse(SAMPLE).unwrap();
        assert_eq!(candidates[0].url.host_str(), Some("good.example"));
        let slow = candidates
            .iter()
            .find(|c| c.url.host_str() == Some("slow.example"))
            .unwrap();
        assert!(candidates[0].score > slow.score);
    }

    #[test]
    fn an_unparseable_directory_is_an_error_not_an_empty_pool() {
        assert!(matches!(parse("not json"), Err(Error::Directory(_))));
        assert!(matches!(
            parse(r#"{"instances": {}}"#),
            Err(Error::Directory(_))
        ));
    }

    #[test]
    fn a_missing_measurement_is_not_a_zero() {
        let bare = score(None, None, None);
        let awful = score(Some(10.0), Some(10.0), Some(10.0));
        assert!(bare > awful);
        assert!(bare < score(Some(0.3), Some(100.0), Some(100.0)));
    }
}
