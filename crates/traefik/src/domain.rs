//! The two strings that end up inside Traefik's own syntax, and the
//! validation that makes them safe to put there.
//!
//! A router rule is an *expression*: `Host(` + the domain + `)`. Pasting an
//! unvalidated string into it is injection with the same shape as SQL —
//! a backtick closes the literal, and what follows can add `||
//! Host(...)` for a domain the caller does not own, or attach middleware.
//! Faber is a multi-user service, so that is a cross-tenant hole rather than
//! a cosmetic one. The defence is in the type: [`Domain`] has exactly one
//! constructor, it rejects every character that is not a letter, digit,
//! hyphen, or label separator, and both `Deserialize` and `FromStr` route
//! through it. There is no way to hold a `Domain` that was never checked.
//!
//! [`Authority`] is the same argument one layer down. It becomes the authority of
//! an upstream URL — `http://<host>:<port>` — where a slash, an `@`, or a
//! space would re-point the upstream somewhere else entirely.
//!
//! Wildcards are rejected. Traefik's `Host` matcher is exact; `*.example.com`
//! needs `HostRegexp`, which is a different rule shape with different
//! escaping, and half-supporting it here would silently produce a router that
//! matches one literal domain nobody will ever request.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

const MAX_NAME: usize = 253;
const MAX_LABEL: usize = 63;

/// A domain that resolves to a target — validated, and lowercased so that
/// `Example.COM` and `example.com` are one entry rather than two routers
/// fighting over the same request.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Domain(String);

impl Domain {
    pub fn new(value: impl AsRef<str>) -> Result<Self> {
        let raw = value.as_ref();
        let lowered = raw.to_ascii_lowercase();
        let reason = check_domain(&lowered);
        match reason {
            Some(reason) => Err(Error::InvalidDomain {
                value: raw.to_owned(),
                reason,
            }),
            None => Ok(Domain(lowered)),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn check_domain(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("must not be empty");
    }
    if value.len() > MAX_NAME {
        return Some("must be at most 253 characters");
    }
    for label in value.split('.') {
        if label.is_empty() {
            return Some("labels must not be empty (no leading, trailing, or doubled dots)");
        }
        if label.len() > MAX_LABEL {
            return Some("each label must be at most 63 characters");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Some("labels must not start or end with a hyphen");
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Some("labels may contain only letters, digits, and hyphens");
        }
    }
    None
}

/// The authority half of an upstream URL: a container name, or the address
/// at which Traefik reaches the host.
///
/// Wider than [`Domain`] because it has to cover what the two callers
/// actually hand over — Docker container names admit `_`, and a host address
/// is often a bare IPv4 literal. Still narrow enough that the result cannot
/// escape the authority position of a URL. IPv6 literals are rejected: they
/// would need bracketing, and nothing in Faber's path produces one today.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Authority(String);

impl Authority {
    pub fn new(value: impl AsRef<str>) -> Result<Self> {
        let raw = value.as_ref();
        match check_authority(raw) {
            Some(reason) => Err(Error::InvalidAuthority {
                value: raw.to_owned(),
                reason,
            }),
            None => Ok(Authority(raw.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn check_authority(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("must not be empty");
    }
    if value.len() > MAX_NAME {
        return Some("must be at most 253 characters");
    }
    if value.starts_with(['-', '.']) || value.ends_with(['-', '.']) {
        return Some("must not start or end with a hyphen or dot");
    }
    if value.contains("..") {
        return Some("must not contain empty labels");
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Some("may contain only letters, digits, hyphens, underscores, and dots");
    }
    None
}

/// A destination port. Zero is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Port(u16);

impl Port {
    pub fn new(port: u16) -> Result<Self> {
        if port == 0 {
            return Err(Error::InvalidPort);
        }
        Ok(Port(port))
    }

    pub fn get(self) -> u16 {
        self.0
    }
}

macro_rules! string_newtype_serde {
    ($ty:ident) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $ty {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = Error;

            fn try_from(value: String) -> Result<Self> {
                Self::new(value)
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        // Validation is not skippable: the only path from JSON to this type
        // runs through the constructor above.
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_newtype_serde!(Domain);
string_newtype_serde!(Authority);

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<u16> for Port {
    type Error = Error;

    fn try_from(port: u16) -> Result<Self> {
        Port::new(port)
    }
}

impl Serialize for Port {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for Port {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Port::new(u16::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_domains() {
        for value in [
            "example.com",
            "a.b.c.example.com",
            "xn--80ak6aa92e.com",
            "localhost",
            "app-1.example.com",
        ] {
            assert!(Domain::new(value).is_ok(), "{value} should be accepted");
        }
    }

    #[test]
    fn rejects_rule_injection_and_malformed_names() {
        for value in [
            // The reason this type exists: each of these would otherwise be
            // pasted verbatim into `Host(`...`)`.
            "example.com`) || Host(`victim.com",
            "example.com`)",
            "a`b.com",
            "exam ple.com",
            "example.com/../x",
            "*.example.com",
            "",
            ".example.com",
            "example..com",
            "example.com.",
            "-example.com",
            "example-.com",
            "user@example.com",
        ] {
            assert!(
                Domain::new(value).is_err(),
                "{value:?} should have been rejected"
            );
        }
    }

    #[test]
    fn rejects_oversized_names() {
        assert!(Domain::new("a".repeat(64) + ".com").is_err());
        let long = std::iter::repeat_n("abcdefgh", 32)
            .collect::<Vec<_>>()
            .join(".");
        assert!(long.len() > MAX_NAME);
        assert!(Domain::new(long).is_err());
    }

    #[test]
    fn lowercases_so_one_domain_is_one_entry() {
        let upper = Domain::new("Example.COM").unwrap();
        assert_eq!(upper.as_str(), "example.com");
        assert_eq!(upper, Domain::new("example.com").unwrap());
    }

    #[test]
    fn authority_accepts_container_names_and_addresses() {
        for value in ["web", "faber_web-1", "172.17.0.1", "host.docker.internal"] {
            assert!(Authority::new(value).is_ok(), "{value} should be accepted");
        }
    }

    #[test]
    fn authority_rejects_anything_that_escapes_it() {
        for value in [
            "web:8080/evil",
            "web/",
            "user@web",
            "web ",
            "",
            "-web",
            "web.",
            "::1",
            "[::1]",
        ] {
            assert!(
                Authority::new(value).is_err(),
                "{value:?} should have been rejected"
            );
        }
    }

    #[test]
    fn port_zero_is_not_a_destination() {
        assert!(Port::new(0).is_err());
        assert_eq!(Port::new(8080).unwrap().get(), 8080);
    }

    #[test]
    fn deserialize_validates() {
        assert!(serde_json::from_str::<Domain>("\"example.com\"").is_ok());
        assert!(serde_json::from_str::<Domain>("\"a`b.com\"").is_err());
        assert!(serde_json::from_str::<Port>("0").is_err());
    }
}
