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

        let connecting = client::connect(config, address, verifier);
        let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => {
                // A refused host key surfaces as a connection error, and it is
                // the one case here that is about trust rather than reach.
                let fingerprint = seen.lock().expect("host key slot poisoned").clone();
                return Err(match (&host_key, fingerprint) {
                    (HostKey::Verify(expected), Some(seen)) if expected != &seen => {
                        Fault::Denied(Denial::Malformed {
                            what: "ssh host key".into(),
                            reason: format!(
                                "`{address}` presented {seen}, but this host is known by \
                                 {expected}; refusing rather than trusting a new key"
                            ),
                        })
                    }
                    _ => Fault::Unreachable(format!("could not reach `{address}`: {error}")),
                });
            }
            Err(_) => {
                return Err(Fault::Unreachable(format!(
                    "`{address}` did not answer within {CONNECT_TIMEOUT:?}"
                )));
            }
        };

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

    pub(crate) fn handle(&self) -> &client::Handle<Verifier> {
        &self.handle
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
        let label = label.into();
        let (session, fingerprint) = SshSession::connect(address, credential, host_key).await?;
        let session = Arc::new(session);

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

        Ok((Machine::new(manifest, spawn, files, blobs), fingerprint))
    }
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
}
