//! Starting a process on the far side, and nothing else.
//!
//! This is one half of the mechanism layer. It knows how to reach a machine
//! and hand back a running process's pipes; it does not know about roots,
//! timeouts, capture limits, blob storage, or capability gates. Those are
//! policy, they are identical in every mode, and they live in
//! [`Machine`](crate::Machine) where a transport cannot reach them to get them
//! wrong.
//!
//! The split is what makes the promise that every mode behaves identically
//! structural rather than a matter of vigilance: there is one implementation
//! of the behavior and several implementations of the plumbing under it.

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::exec::{Outcome, Signal};
use crate::fault::Fault;

/// One process, fully resolved.
///
/// Deliberately not [`Exec`](crate::Exec): no timeout, no capture, no
/// [`RootedPath`](crate::RootedPath). `cwd` is already confined and is the
/// path as the *target* spells it, because that is what a transport can use
/// and the agent-facing spelling is not.
#[derive(Clone, Debug)]
pub struct Run {
    /// The full argv, shell included. The shell binary comes from the
    /// manifest, so choosing it is policy and arriving here it is settled.
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    /// A terminal rather than a pipe. Only modes publishing
    /// [`Capability::Pty`](crate::Capability) are ever asked for one.
    pub pty: bool,
}

/// A process's stdin.
pub type Sink = Box<dyn AsyncWrite + Send + Unpin>;
/// One of a process's output streams.
pub type Source = Box<dyn AsyncRead + Send + Unpin>;

/// Reach the machine: start one process, get its pipes back.
#[async_trait]
pub trait Spawn: Send + Sync {
    async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault>;
}

/// A process that was started and has not yet been accounted for.
///
/// The pipe accessors take rather than borrow — each returns `Some` once and
/// `None` afterward — because a caller that reads a stream twice gets each
/// byte once, split across the two readers, and that is a bug worth making
/// unrepresentable.
///
/// `Send` but not `Sync`: a process is owned by one holder at a time, and the
/// pipe types under it are not shareable anyway. Concurrent access is the
/// registry's problem and it already holds these behind a lock.
#[async_trait]
pub trait Proc: Send {
    fn stdin(&mut self) -> Option<Sink>;
    fn stdout(&mut self) -> Option<Source>;
    fn stderr(&mut self) -> Option<Source>;

    /// Waits for termination. Blocking here is the caller's choice: the
    /// timeout is policy and is applied above this trait.
    async fn wait(&mut self) -> Result<Outcome, Fault>;

    /// Reaps if it can, without blocking. `None` means still running, which is
    /// what tells a reader to come back.
    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault>;

    /// Process lifecycle only. Never the container's, and never the host's.
    ///
    /// Signalling a process that has already been reaped is refused above this
    /// trait, where the handle is known — its pid is free for the OS to hand
    /// to someone else, and the implementation cannot tell whose it is now.
    async fn signal(&mut self, signal: Signal) -> Result<(), Fault>;
}
