//! What a caller stores: one domain, one target.
//!
//! The two target kinds differ in *who owns the port*, and that is the whole
//! distinction. A container target names a container and a port inside it,
//! and the authority Traefik dials is the container name — Docker's embedded
//! DNS resolves it, provided the operator put Traefik and the container on a
//! shared network. A host target names only a port, because the thing
//! listening is on the host itself and there is no container to address.
//!
//! Nothing here asks whether the port is open, whether the container exists,
//! or whether Traefik has a route to either. A target that is currently
//! unreachable is a valid entry that produces a 502 — which is the right
//! behaviour for a service that has not started yet, and the wrong one to
//! turn into a write error.

use serde::{Deserialize, Serialize};

use crate::config::Placement;
use crate::domain::{Authority, Domain, Port};
use crate::error::Result;

/// Where a domain sends its traffic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// A port inside a named container, reached by container name.
    Container { container: Authority, port: Port },
    /// A port on the host Traefik is serving for. Resolves nowhere near a
    /// container; see [`Placement`] for how the address is chosen.
    Host { port: Port },
}

impl Target {
    pub fn container(container: impl AsRef<str>, port: u16) -> Result<Self> {
        Ok(Target::Container {
            container: Authority::new(container)?,
            port: Port::new(port)?,
        })
    }

    pub fn host(port: u16) -> Result<Self> {
        Ok(Target::Host {
            port: Port::new(port)?,
        })
    }

    pub fn port(&self) -> Port {
        match self {
            Target::Container { port, .. } | Target::Host { port } => *port,
        }
    }

    /// The upstream URL Traefik will dial.
    ///
    /// Always `http` — TLS is terminated at Traefik, and re-encrypting the
    /// last hop is a decision with certificate consequences that belongs to
    /// whoever configures the entry points, not to a resolution table.
    pub fn upstream(&self, placement: &Placement) -> String {
        match self {
            Target::Container { container, port } => format!("http://{container}:{port}"),
            Target::Host { port } => format!("http://{}:{port}", placement.host_address()),
        }
    }
}

/// One resolution: `domain` → `target`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub domain: Domain,
    pub target: Target,
}

impl Entry {
    pub fn new(domain: Domain, target: Target) -> Self {
        Entry { domain, target }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_targets_are_dialled_by_name_under_either_placement() {
        let target = Target::container("web", 8080).unwrap();
        assert_eq!(
            target.upstream(&Placement::Host),
            "http://web:8080",
            "the operator owns reachability; we never rewrite a container name"
        );
        assert_eq!(
            target.upstream(&Placement::docker("172.17.0.1").unwrap()),
            "http://web:8080"
        );
    }

    #[test]
    fn host_targets_follow_the_placement() {
        let target = Target::host(3000).unwrap();
        assert_eq!(target.upstream(&Placement::Host), "http://127.0.0.1:3000");
        assert_eq!(
            target.upstream(&Placement::docker("host.docker.internal").unwrap()),
            "http://host.docker.internal:3000"
        );
    }

    #[test]
    fn rejects_a_container_name_that_would_escape_the_url() {
        assert!(Target::container("web/../evil", 80).is_err());
        assert!(Target::container("web", 0).is_err());
    }

    #[test]
    fn serde_roundtrip_keeps_the_kind_tag() {
        let entry = Entry::new(
            Domain::new("example.com").unwrap(),
            Target::container("web", 8080).unwrap(),
        );
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"kind\":\"container\""));
        assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), entry);
    }
}
