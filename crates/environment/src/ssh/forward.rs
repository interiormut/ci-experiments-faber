//! A container daemon on the far side of an SSH session.
//!
//! This is the whole of `ssh+docker`. Everything about talking to a daemon —
//! exec, archive, framing, signals — is already written against a stream, so
//! reaching a remote daemon is a different way of opening one and nothing
//! else. `direct-streamlocal@openssh.com` dials a unix socket on the far side
//! and hands back a channel, which is what `DOCKER_HOST=ssh://` does
//! internally, minus the CLI and minus the ambient configuration.
//!
//! Notably this does *not* require the remote daemon to listen on TCP. Asking
//! users to expose a docker socket over the network in order to be managed
//! would be trading a large amount of their security for a little of our
//! convenience.

use async_trait::async_trait;
use std::sync::Arc;

use crate::docker::daemon::{Conn, Daemon};
use crate::fault::Fault;
use crate::ssh::SshSession;

/// The default location of a docker socket on a unix host.
pub const DOCKER_SOCKET: &str = "/var/run/docker.sock";

pub struct SshForwarded {
    session: Arc<SshSession>,
    socket: String,
}

impl SshForwarded {
    /// `socket` is the daemon's path *on the far side*. Rootless daemons live
    /// somewhere else entirely, so this is configuration rather than a
    /// constant with an override.
    pub fn new(session: Arc<SshSession>, socket: impl Into<String>) -> Self {
        SshForwarded {
            session,
            socket: socket.into(),
        }
    }
}

#[async_trait]
impl Daemon for SshForwarded {
    async fn connect(&self) -> Result<Conn, Fault> {
        let channel = self
            .session
            .handle()
            .channel_open_direct_streamlocal(self.socket.clone())
            .await
            .map_err(|error| {
                Fault::Unreachable(format!(
                    "could not reach `{}` on the far side: {error}",
                    self.socket
                ))
            })?;
        Ok(Box::new(channel.into_stream()))
    }

    fn describe(&self) -> String {
        format!("ssh:{}", self.socket)
    }
}
