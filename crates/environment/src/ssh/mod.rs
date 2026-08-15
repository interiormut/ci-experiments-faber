//! Execution on another machine, over SSH.
//!
//! One session serves everything: commands run on their own channels, files
//! move over the SFTP subsystem on another, and a container daemon on the far
//! side is reached by forwarding to its socket over a third. So an SSH host
//! used in docker mode still costs one connection and one authentication.
//!
//! **Everything is passed in at bind.** The address, the user, the key
//! material, and the expected host key are all arguments. Nothing here reads
//! `~/.ssh/config`, `known_hosts`, an agent socket, or any environment
//! variable — on a shared deployment those describe the operator, and
//! borrowing them would send a user's connection out under the operator's
//! identity and the operator's trust decisions.
//!
//! Two consequences worth stating plainly:
//!
//! - **Host keys are checked against something the caller supplied**, because
//!   there is no file here to consult. [`HostKey::AcceptNew`] exists for the
//!   first connection and reports what it saw so the caller can store it;
//!   after that, [`HostKey::Verify`] is the whole point.
//! - **Background handles die with the session.** Reattaching after a dropped
//!   connection needs a supervisor process on the far side, which is
//!   lifecycle, and lifecycle is not Faber's. A call against a dead handle
//!   reports that the machine went away, and the agent restarts the work.

pub mod files;
pub mod forward;
pub mod spawn;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key, ssh_key};
use tokio::io::{AsyncRead, AsyncWrite};

pub use files::SftpFiles;
pub use forward::SshForwarded;
pub use spawn::SshSpawn;

use crate::fault::{Denial, Fault};
use crate::machine::Machine;
use crate::manifest::{Capability, Manifest, Posture, Reachability, Scope};
use crate::path::Root;
use crate::probe::{SHELL, probe};
use crate::registry::Label;
use crate::store::Blobs;

/// How long to wait for the far side to answer at all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Who to log in as, and with what.
///
/// Key *material*, not a reference to a secret store. Resolving a handle to a
/// key is the consumer's job — this crate does not know what a secret store
/// is, and teaching it would point a dependency the wrong way.
pub struct SshCredential {
    pub user: String,
    /// An OpenSSH private key, in the format the file itself is in.
    pub private_key: String,
    pub passphrase: Option<String>,
}

/// What to do about the far side's host key.
///
/// There is no third option. Accepting whatever answers is a silent
/// machine-in-the-middle on every connection, and in a service holding many
/// users' credentials that is the one failure that compromises everyone at
/// once.
#[derive(Clone, Debug)]
pub enum HostKey {
    /// The fingerprint this host is known by, as `SHA256:…`. Anything else is
    /// refused.
    Verify(String),
    /// First contact: accept what answers and report it, so the caller can
    /// store it and verify from then on.
    AcceptNew,
}

/// Checks the server's key against what the caller said to expect.
pub struct Verifier {
    expected: HostKey,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Verifier {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let fingerprint = key.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        *self.seen.lock().expect("host key slot poisoned") = Some(fingerprint.clone());

        Ok(match &self.expected {
            HostKey::AcceptNew => true,
            HostKey::Verify(expected) => expected == &fingerprint,
        })
    }
}

/// One authenticated connection, shared by every channel opened over it.
pub struct SshSession {
    handle: client::Handle<Verifier>,
}

impl SshSession {
    /// Connects and authenticates.
    ///
    /// Returns the session and the host key it saw, so a caller using
    /// [`HostKey::AcceptNew`] has something to persist and verify against
    /// next time.
    pub async fn connect(
        address: &str,
        credential: &SshCredential,
        host_key: HostKey,
    ) -> Result<(Self, String), Fault> {
        let key = decode_secret_key(
            &credential.private_key,
            credential.passphrase.as_deref(),
        )
            .map_err(|error| {
                Fault::Denied(Denial::Malformed {
                    what: "ssh key".into(),
                    reason: format!("could not read the private key: {error}"),
                })
            })?;

        let seen = Arc::new(Mutex::new(None));
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            ..Default::default()
        });

        let verifier = Verifier {
            expected: host_key.clone(),
            seen: Arc::clone(&seen),
        };

        let mut handle = Self::dial(
            client::connect(config, address, verifier),
            address,
            &host_key,
            &seen,
        )
        .await?;

        let authenticated = handle
            .authenticate_publickey(
                &credential.user,
                PrivateKeyWithHashAlg::new(Arc::new(key), None),
            )
            .await
            .map_err(|error| Fault::Unreachable(format!("authentication failed: {error}")))?;

        if !authenticated.success() {
            return Err(Fault::Denied(Denial::Malformed {
                what: "ssh credential".into(),
                reason: format!("`{}` was not accepted by `{address}`", credential.user),
            }));
        }

        let fingerprint = seen
            .lock()
            .expect("host key slot poisoned")
            .clone()
            .unwrap_or_default();

        Ok((SshSession { handle }, fingerprint))
    }

    /// Connects over a stream that is already open — no dial, because for an
    /// agent-transport host there is nothing to dial (R14). `name` stands in
    /// for the address that [`Fault`] messages would otherwise quote.
    ///
    /// Takes no [`SshCredential`]: the daemon on the far side of an agent
    /// stream is not authenticating a human. It already proved itself one
    /// layer below the SSH handshake, over the connection it dialed out to
    /// establish (R15), so the SSH layer here is deliberately not a trust
    /// boundary — it authenticates with `auth_none`, on the strength of that
    /// prior check.
    pub async fn from_stream<S>(
        stream: S,
        name: &str,
        host_key: HostKey,
    ) -> Result<(Self, String), Fault>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let seen = Arc::new(Mutex::new(None));
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            ..Default::default()
        });

        let verifier = Verifier {
            expected: host_key.clone(),
            seen: Arc::clone(&seen),
        };

        let mut handle = Self::dial(
            client::connect_stream(config, stream, verifier),
            name,
            &host_key,
            &seen,
        )
        .await?;

        // There is no user to name — the peer accepts unconditionally
        // (X39) — so this string is a label on the wire, not a credential.
        let authenticated = handle
            .authenticate_none("faber")
            .await
            .map_err(|error| Fault::Unreachable(format!("authentication failed: {error}")))?;

        if !authenticated.success() {
            return Err(Fault::Denied(Denial::Malformed {
                what: "agent session".into(),
                reason: format!("`{name}` refused an unauthenticated session"),
            }));
        }

        let fingerprint = seen
            .lock()
            .expect("host key slot poisoned")
            .clone()
            .unwrap_or_default();

        Ok((SshSession { handle }, fingerprint))
    }

    /// Waits for the transport to come up and checks the presented host key,
    /// shared by [`Self::connect`] (dialing) and [`Self::from_stream`] (an
    /// already-open stream). `name` is what [`Fault`] messages quote — an
    /// address for one, a display label for the other.
    async fn dial<F>(
        connecting: F,
        name: &str,
        host_key: &HostKey,
        seen: &Arc<Mutex<Option<String>>>,
    ) -> Result<client::Handle<Verifier>, Fault>
    where
        F: std::future::Future<Output = Result<client::Handle<Verifier>, russh::Error>>,
    {
        match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(Ok(handle)) => Ok(handle),
            Ok(Err(error)) => {
                // A refused host key surfaces as a connection error, and it is
                // the one case here that is about trust rather than reach.
                let fingerprint = seen.lock().expect("host key slot poisoned").clone();
                Err(match (host_key, fingerprint) {
                    (HostKey::Verify(expected), Some(seen)) if expected != &seen => {
                        Fault::Denied(Denial::Malformed {
                            what: "ssh host key".into(),
                            reason: format!(
                                "`{name}` presented {seen}, but this host is known by \
                                 {expected}; refusing rather than trusting a new key"
                            ),
                        })
                    }
                    _ => Fault::Unreachable(format!("could not reach `{name}`: {error}")),
                })
            }
            Err(_) => Err(Fault::Unreachable(format!(
                "`{name}` did not answer within {CONNECT_TIMEOUT:?}"
            ))),
        }
    }

    pub(crate) fn handle(&self) -> &client::Handle<Verifier> {
        &self.handle
    }

    /// Whether the underlying connection has already gone down — a dropped
    /// TCP socket, a completed [`Self::disconnect`], or the far side closing
    /// first. A caller holding a shared session (R16) checks this rather
    /// than dialing to find out, because for an agent-transport host there
    /// is nothing to dial (R14).
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// Closes the connection out from under every clone of this session.
    ///
    /// Dropping an `Arc<SshSession>` only closes the underlying handle once
    /// the *last* clone goes away — no help to a caller preempting a shared
    /// session (R16) while other bindings still hold a reference to it. This
    /// tears the transport down regardless of who else is holding on, so
    /// every one of them fails on its next call rather than silently
    /// outliving the connection they think they still have.
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
}

/// Execution directly on another machine.
///
/// A constructor, not a type — what it returns is a [`Machine`].
pub struct SshTarget;

impl SshTarget {
    /// Probes and binds, and reports the host key it saw.
    ///
    /// The fingerprint comes back on every bind, not only the first: a caller
    /// storing it can compare without a second round trip, and a caller that
    /// passed [`HostKey::Verify`] already knows it matched because the
    /// connection succeeded.
    pub async fn bind(
        label: impl Into<Label>,
        address: &str,
        credential: &SshCredential,
        host_key: HostKey,
        root: Root,
        blobs: Arc<dyn Blobs>,
    ) -> Result<(Machine, String), Fault> {
        let (session, fingerprint) = SshSession::connect(address, credential, host_key).await?;
        let machine = Self::bind_session(label, Arc::new(session), root, blobs).await?;
        Ok((machine, fingerprint))
    }

    /// Binds against a session the caller already holds, rather than dialing
    /// one — what an agent-transport host uses (X39): the connection came in
    /// from the daemon, is kept by the broker, and every bind against that
    /// host shares it rather than opening a second one. There is no
    /// fingerprint to return here because there is nothing to write back —
    /// an agent host's host key was pinned at enrollment, not learned here.
    pub async fn bind_session(
        label: impl Into<Label>,
        session: Arc<SshSession>,
        root: Root,
        blobs: Arc<dyn Blobs>,
    ) -> Result<Machine, Fault> {
        let label = label.into();

        let files = Arc::new(SftpFiles::open(Arc::clone(&session), root.clone()).await?);
        let spawn = Arc::new(SshSpawn::new(Arc::clone(&session), root.as_str()));

        let probed = probe(spawn.as_ref(), root.as_str().to_owned()).await?;

        // Same set as direct execution on this machine: an SSH channel carries
        // stdin over a pipe just as a child process does. Pty is not
        // advertised for the same reason it is not there — nothing here
        // requests a terminal, so promising one would be promising a code path
        // that does not exist.
        let capabilities = BTreeSet::from([
            Capability::Exec,
            Capability::Background,
            Capability::Stdin,
            Capability::Read,
            Capability::Write,
            Capability::Edit,
            Capability::Patch,
            Capability::List,
        ]);

        let agent_env = BTreeMap::from([
            ("FABER_AGENT".to_owned(), "1".to_owned()),
            ("FABER_TARGET".to_owned(), label.0.clone()),
        ]);

        let manifest = Manifest {
            label,
            os: probed.os,
            arch: probed.arch,
            shell: SHELL.to_owned(),
            root,
            tools: probed.tools,
            capabilities,
            // A directory on a machine that carries much more. The boundary is
            // this API's alone.
            scope: Scope::Workspace,
            network: Reachability::Unknown,
            // Direct on a host: this API refuses escape and nothing
            // underneath does.
            posture: Posture::Conventional,
            agent_env,
            login_shell_sourced: false,
            probed_at: SystemTime::now(),
        };

        Ok(Machine::new(manifest, spawn, files, blobs))
    }
}

/// Computes the `SHA256:…` fingerprint [`HostKey::Verify`] expects from an
/// OpenSSH public key line, the format a daemon reports at enrollment.
///
/// `Verifier::check_server_key` computes the same fingerprint off the key
/// the far side actually presents; this is the other half, turning what an
/// agent host pinned at enrollment into something comparable to it.
pub fn fingerprint_of(public_key: &str) -> Result<String, Fault> {
    ssh_key::PublicKey::from_openssh(public_key)
        .map(|key| key.fingerprint(ssh_key::HashAlg::Sha256).to_string())
        .map_err(|error| {
            Fault::Denied(Denial::Malformed {
                what: "agent host key".into(),
                reason: format!("could not read the public key: {error}"),
            })
        })
}

/// Makes a string safe to sit inside single quotes in a remote command.
///
/// SSH runs one command *string*, not an argv, so everything that crosses has
/// to survive the far side's shell exactly once.
pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_a_quote_in_the_value() {
        assert_eq!(quote("/work/it's.txt"), r"'/work/it'\''s.txt'");
        assert_eq!(quote("plain"), "'plain'");
    }

    #[test]
    fn a_public_keys_fingerprint_matches_what_ssh_keygen_reports() {
        // Generated with `ssh-keygen -t ed25519` and checked against
        // `ssh-keygen -lf … -E sha256` — the value an agent host's
        // `HostKey::Verify` is pinned to at enrollment (X39) has to be the
        // same one any operator comparing keys by hand would compute.
        let public_key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIML17Ogk1BkPexAPb4jAfCUfFq0h9LzAhi09kSznnjY0 test";
        assert_eq!(
            fingerprint_of(public_key).unwrap(),
            "SHA256:wAqPrmRonvVAwAh0uRdXIx08v3fotn2rI8/9Rym1dlo"
        );
    }

    #[test]
    fn an_unreadable_public_key_is_a_malformed_denial() {
        let err = fingerprint_of("not a key").unwrap_err();
        assert!(matches!(
            err,
            Fault::Denied(Denial::Malformed { what, .. }) if what == "agent host key"
        ));
    }
}
