//! Domain → target resolution, expressed as Traefik dynamic configuration.
//!
//! One [`Traefik`] instance owns one dynamic-configuration file and offers
//! create, read, update, delete over [`Entry`] values. An entry is a
//! [`Domain`] and a [`Target`]; the target is either a port inside a named
//! container or a port on the host, and every write rewrites the whole
//! document so the file always states the current entry set exactly.
//!
//! What this crate deliberately does not do:
//!
//! - **It does not check reachability.** Whether Traefik shares a Docker
//!   network with the container, whether the host port is bound, whether the
//!   dynamic-configuration directory is mounted where Traefik expects — all
//!   of that is the operator's, and none of it is observable from here. An
//!   entry for something that is not listening yet is a valid entry; it
//!   yields a 502 until the thing starts, which is the correct behaviour for
//!   a service that comes up later and the wrong thing to reject at write
//!   time.
//! - **It does not talk to Docker or to Traefik.** No socket, no API client,
//!   no reload call. The file provider watches its path and reloads on its
//!   own within a couple of seconds; the entire mechanism is one file.
//! - **It does not read the environment.** Everything deployment-specific
//!   arrives through [`Config`]. Faber serves many users from one process,
//!   so an ambient default here would become every user's default.
//! - **It has no notion of who owns a domain, and therefore authorizes
//!   nothing.** A domain is a global name here: whoever calls [`Traefik::put`]
//!   or [`Traefik::update`] wins, and [`Traefik::create`]'s conflict check is
//!   about uniqueness, not about tenancy. The caller must resolve *this user
//!   owns this domain* before every mutating call — a route handler that
//!   forwards a request body straight into this crate lets one user repoint
//!   another user's domain at their own container.
//!
//! The load-bearing pieces:
//!
//! - [`Domain`] — validated at its only constructor, because the domain is
//!   pasted into a Traefik rule expression and an unchecked one is injection
//!   across tenants. [`Authority`] is the same argument for the authority of an
//!   upstream URL.
//! - [`Placement`] — the only thing that changes with where Traefik runs, and
//!   it changes exactly one value: the address a host target resolves to.
//!   Container targets are dialled by name either way.
//! - [`Traefik`] — the CRUD surface, serialized against the file it owns.
//!
//! ```no_run
//! # async fn example() -> Result<(), traefik::Error> {
//! use traefik::{Config, Domain, Entry, Placement, Target, Traefik};
//!
//! let manager = Traefik::new(
//!     Config::new("/etc/traefik/dynamic/faber.yml", Placement::docker("172.17.0.1")?)
//!         .with_cert_resolver("letsencrypt"),
//! );
//! // Startup: the caller's database is the durable copy.
//! manager.replace([]).await?;
//!
//! manager
//!     .create(Entry::new(
//!         Domain::new("app.example.com")?,
//!         Target::container("faber-app-1", 8080)?,
//!     ))
//!     .await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod domain;
pub mod entry;
pub mod error;
pub mod manager;
mod render;

pub use config::{Config, DEFAULT_ENTRY_POINT, DEFAULT_NAME_PREFIX, Placement};
pub use domain::{Authority, Domain, Port};
pub use entry::{Entry, Target};
pub use error::{Error, Result};
pub use manager::Traefik;
