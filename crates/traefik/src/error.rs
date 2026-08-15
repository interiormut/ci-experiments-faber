//! Failures, split so the consumer never has to match on a message.
//!
//! The variants are shaped for the eventual HTTP mapping: the `Invalid*`
//! family is a bad request, [`Error::AlreadyExists`] is a conflict,
//! [`Error::NotFound`] is a miss, and [`Error::Io`] is the only variant that
//! means "the operator's filesystem said no". Nothing here carries a
//! reachability verdict, because this crate never probes one.

use std::path::PathBuf;

use crate::domain::Domain;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The string is not a hostname this crate is willing to paste into a
    /// Traefik rule. See [`Domain`] for what is accepted and why.
    #[error("invalid domain `{value}`: {reason}")]
    InvalidDomain { value: String, reason: &'static str },

    /// The string is not usable as the authority of an upstream URL — a
    /// container name, or the host address Traefik dials for a host target.
    #[error("invalid host `{value}`: {reason}")]
    InvalidAuthority { value: String, reason: &'static str },

    /// Port 0 is not a destination.
    #[error("port must be between 1 and 65535")]
    InvalidPort,

    #[error("domain `{domain}` already has an entry")]
    AlreadyExists { domain: Domain },

    #[error("no entry for domain `{domain}`")]
    NotFound { domain: Domain },

    #[error("writing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Serializing the dynamic configuration. Unreachable in practice — the
    /// document is plain maps of strings — but not worth a panic.
    #[error("rendering the dynamic configuration: {0}")]
    Render(#[from] serde_json::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
