//! How this crate is wired to somebody else's Traefik.
//!
//! Two things vary between deployments and neither can be discovered from
//! inside this process: where the dynamic configuration file lives, and where
//! Traefik is running relative to the host whose ports a host target names.
//! Both are supplied by the operator through [`Config`]. Nothing is read from
//! the ambient environment — no `DOCKER_HOST`, no probing of `/proc`, no
//! guessing a bridge address — because Faber serves many users from one
//! process and an ambient default would silently become every user's default.

use std::path::{Path, PathBuf};

use crate::domain::Authority;
use crate::error::Result;

/// Where Traefik runs, which decides only one thing: the address at which a
/// *host* target's port is reachable.
///
/// Container targets are unaffected. Under either placement they are dialled
/// by container name, because rewriting them into published host ports would
/// be this crate guessing at a network topology it was told not to care
/// about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Traefik is a process on the host. The host's own ports are loopback.
    Host,
    /// Traefik is in a container, so the host is a different network
    /// namespace and needs an address.
    ///
    /// There is no default worth having. `host.docker.internal` does not
    /// exist on Linux unless the container was started with
    /// `--add-host=host.docker.internal:host-gateway`, and the bridge gateway
    /// address is whatever the operator's Docker networks say it is. Making
    /// the operator name it keeps the failure at configuration time.
    Docker { host_address: Authority },
}

impl Placement {
    pub fn docker(host_address: impl AsRef<str>) -> Result<Self> {
        Ok(Placement::Docker {
            host_address: Authority::new(host_address)?,
        })
    }

    /// The authority a host target resolves to.
    pub fn host_address(&self) -> &str {
        match self {
            Placement::Host => "127.0.0.1",
            Placement::Docker { host_address } => host_address.as_str(),
        }
    }
}

/// The default entry point a generated router listens on.
///
/// A convention, not a Traefik built-in: the operator has to have declared it
/// in static configuration (`--entryPoints.websecure.address=:443`). Naming
/// one that does not exist fails visibly rather than quietly — the router
/// appears in the dashboard with status `disabled` and the error `entryPoint
/// "websecure" doesn't exist`, checked against `traefik:v3` — so a
/// mismatched deployment is diagnosable from Traefik's own API.
pub const DEFAULT_ENTRY_POINT: &str = "websecure";

/// Prefixed onto every generated router and service name, so a glance at
/// Traefik's dashboard says which entries are Faber's.
pub const DEFAULT_NAME_PREFIX: &str = "faber-";

/// Everything the manager needs that only the operator knows.
#[derive(Clone, Debug)]
pub struct Config {
    /// The file this crate owns, rewritten in full on every change.
    ///
    /// Point either `providers.file.filename` or `providers.file.directory`
    /// at it — in directory mode the crate's temporary file is named so
    /// Traefik skips it (see [`crate::manager`]). Nothing else may write
    /// this file; other files in the same directory are untouched.
    ///
    /// The extension must be one Traefik's file provider recognises —
    /// `.yml`, `.yaml`, or `.toml`. It is *not* free-form: the provider
    /// selects a decoder by extension and silently skips anything else,
    /// logging only at debug level. Use `.yml`: the document is rendered as
    /// JSON, which Traefik's YAML decoder accepts, and the `render` module
    /// explains the trade.
    pub file: PathBuf,

    /// Where Traefik runs, for host targets.
    pub placement: Placement,

    /// Entry points every generated router listens on. Operator-supplied
    /// names from static configuration, never user input.
    pub entry_points: Vec<String>,

    /// ACME resolver for the generated routers, if the operator uses one.
    pub cert_resolver: Option<String>,

    /// Whether generated routers terminate TLS. On by default: a domain
    /// pointed at Faber is served over HTTPS, and an entry that quietly
    /// listened in cleartext would be a worse surprise than a certificate
    /// error.
    pub tls: bool,

    /// Prefix for generated router and service names.
    pub name_prefix: String,
}

impl Config {
    pub fn new(file: impl Into<PathBuf>, placement: Placement) -> Self {
        Config {
            file: file.into(),
            placement,
            entry_points: vec![DEFAULT_ENTRY_POINT.to_owned()],
            cert_resolver: None,
            tls: true,
            name_prefix: DEFAULT_NAME_PREFIX.to_owned(),
        }
    }

    pub fn with_entry_points<S: Into<String>>(
        mut self,
        entry_points: impl IntoIterator<Item = S>,
    ) -> Self {
        self.entry_points = entry_points.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_cert_resolver(mut self, resolver: impl Into<String>) -> Self {
        self.cert_resolver = Some(resolver.into());
        self
    }

    pub fn without_tls(mut self) -> Self {
        self.tls = false;
        self
    }

    pub fn with_name_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_prefix = prefix.into();
        self
    }

    pub fn file(&self) -> &Path {
        &self.file
    }
}
