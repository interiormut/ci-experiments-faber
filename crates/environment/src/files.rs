//! Moving bytes and reading metadata on the far side.
//!
//! The other half of the mechanism layer. Everything with a decision in it —
//! the line window, the edit precondition, the glob, the listing cap and its
//! flag, patch sequencing — is policy and lives in [`Machine`](crate::Machine).
//! What is left here is the part only a transport can do.
//!
//! **No implementation may reach for the target's own coreutils.** Shelling
//! out to `cat`, `sed`, or `ls` makes behavior a property of whatever the
//! image happens to carry, and busybox-versus-GNU is a real difference waiting
//! in every alpine container. Talking to an SSH server or a container daemon
//! is fine and is the point: those are versioned things Faber speaks to
//! deliberately, not whatever a `Dockerfile` left behind.

use async_trait::async_trait;

use crate::fault::Fault;
use crate::file::{EntryKind, Stat};
use crate::path::RootedPath;

/// Where a path lands on the target, and what is there.
#[derive(Clone, Debug)]
pub struct Confined {
    /// The target-side absolute path after resolution. Transport-facing, and
    /// never rendered to the agent.
    pub path: String,
    /// `None` when nothing exists there yet, so a `write` to a new file is
    /// confined by the same check as a `read` of an existing one.
    pub kind: Option<EntryKind>,
}

/// One directory entry, as the transport found it.
///
/// A name rather than a path: joining it onto a [`RootedPath`] is subject to
/// the same refusals as any other path, so it happens once, above this trait,
/// rather than in each implementation.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
}

/// Bytes and metadata on the far side.
#[async_trait]
pub trait Files: Send + Sync {
    /// Resolves a path and refuses anything that leaves the root.
    ///
    /// The lexical half already happened in
    /// [`RootedPath::new`](crate::RootedPath::new); this is the half a lexical
    /// check cannot do, because it cannot see a symlink. What that costs
    /// differs per mode, and the difference is exactly what
    /// [`Posture`](crate::Posture) describes: where the substrate enforces the
    /// boundary there is nothing left for this to do, and saying so is honest
    /// rather than lazy.
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault>;

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault>;

    /// Writes a file whole, creating it and any missing parents.
    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault>;

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault>;

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault>;

    /// One directory's entries, unfiltered and unsorted. The glob, the cap,
    /// and the truncation flag are applied above.
    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault>;
}
