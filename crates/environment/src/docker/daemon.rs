//! Where the Engine API is, and how to open a stream to it.
//!
//! This is the axis the transport varies along while the exec mode stays put.
//! `local+docker` and `ssh+docker` differ *only* in which implementation of
//! this trait they hold: one dials a socket on this machine, the other dials
//! the same socket through an SSH channel. Everything above — exec, archive,
//! framing — is written against the stream and does not know which it got.
//!
//! The endpoint is always configuration passed in at bind. Nothing here reads
//! `DOCKER_HOST` or a docker context: on a shared deployment those belong to
//! the operator, and a user's container is not reachable at the operator's
//! endpoint by coincidence.

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::fault::Fault;

/// A bidirectional byte stream. The Engine API needs both halves — an exec
/// carries stdin up the same connection its output comes down.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// One connection to a daemon.
pub type Conn = Box<dyn Io>;

/// Something that can be dialled to reach an Engine API.
///
/// A fresh connection per call, deliberately: an exec holds its connection
/// open for the life of the process, and inspecting that same exec needs
/// another one. Pooling is an optimization that would have to be correct about
/// hijacked connections, which never return to a pool.
#[async_trait]
pub trait Daemon: Send + Sync {
    async fn connect(&self) -> Result<Conn, Fault>;

    /// For error messages. Never credentials.
    fn describe(&self) -> String;
}

/// A daemon reachable from the Faber process directly — a unix socket, or TCP.
///
/// This is the `local` half of `local+docker`. The `ssh` half is the same
/// Engine API over a channel, and lands with the SSH transport.
pub struct LocalSocket {
    endpoint: String,
}

impl LocalSocket {
    /// `unix:///path/to/sock` or `tcp://host:port`.
    ///
    /// No default. A daemon Faber was not told about is one it should not be
    /// talking to, and "null means the local socket" is exactly the ambient
    /// resolution this crate is avoiding.
    pub fn new(endpoint: impl Into<String>) -> Result<Self, Fault> {
        let endpoint = endpoint.into();
        if !endpoint.starts_with("unix://") && !endpoint.starts_with("tcp://") {
            return Err(Fault::Denied(crate::fault::Denial::Malformed {
                what: "docker endpoint".into(),
                reason: format!("`{endpoint}` is neither a `unix://` nor a `tcp://` endpoint"),
            }));
        }
        Ok(LocalSocket { endpoint })
    }
}

#[async_trait]
impl Daemon for LocalSocket {
    async fn connect(&self) -> Result<Conn, Fault> {
        if let Some(path) = self.endpoint.strip_prefix("unix://") {
            let stream = UnixStream::connect(path).await.map_err(|error| {
                Fault::Unreachable(format!(
                    "could not reach the docker daemon at {path}: {error}"
                ))
            })?;
            Ok(Box::new(stream))
        } else {
            let address = self.endpoint.trim_start_matches("tcp://");
            let stream = TcpStream::connect(address).await.map_err(|error| {
                Fault::Unreachable(format!(
                    "could not reach the docker daemon at {address}: {error}"
                ))
            })?;
            Ok(Box::new(stream))
        }
    }

    fn describe(&self) -> String {
        self.endpoint.clone()
    }
}
