//! `host_id -> AgentLink`, in-memory, one process's view only.
//!
//! Single-replica only (X43): a bind dispatched to a process that never saw
//! this daemon's connection has nothing to look up. The cross-replica story
//! is out of scope for this registry — see X43/X46.1.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use environment::ssh::SshSession;
use uuid::Uuid;

/// One connected daemon's session, shared by every binding and every
/// concurrent run against that host (R16) — reused rather than reopened,
/// since there is nothing here to reopen (R14).
#[derive(Default)]
pub struct AgentRegistry {
    links: Mutex<HashMap<Uuid, Arc<SshSession>>>,
}

impl AgentRegistry {
    /// Registers a freshly connected session, replacing and closing whatever
    /// was there before.
    ///
    /// R16 — newest wins, unconditionally: two live connections claiming the
    /// same host is a routing ambiguity, not a capacity question. The old
    /// session is disconnected explicitly rather than simply dropped, so
    /// every binding still holding a clone of it fails on its next call
    /// instead of quietly outliving a connection that no longer exists on
    /// the wire — see `SshSession::disconnect`.
    pub async fn adopt(&self, host_id: Uuid, session: SshSession) -> Arc<SshSession> {
        let session = Arc::new(session);
        let previous = {
            let mut links = self.links.lock().expect("agent registry poisoned");
            links.insert(host_id, Arc::clone(&session))
        };
        if let Some(previous) = previous {
            tracing::info!(%host_id, "agent connection replaced by a newer one");
            previous.disconnect().await;
        }
        session
    }

    /// The live session for a host, or `None` if no daemon is connected —
    /// which is the whole of what `Unreachable` means for this transport
    /// (R14): there is no dial to retry.
    ///
    /// Eviction is lazy rather than watched by a background task: a session
    /// whose connection died — the far side closed it, or a missed ping
    /// deadline broke the stream (see `agent::link`) — is removed the next
    /// time something asks for it, which is the only time staleness matters.
    pub fn get(&self, host_id: Uuid) -> Option<Arc<SshSession>> {
        let mut links = self.links.lock().expect("agent registry poisoned");
        match links.get(&host_id) {
            Some(session) if session.is_closed() => {
                links.remove(&host_id);
                None
            }
            Some(session) => Some(Arc::clone(session)),
            None => None,
        }
    }

    /// Closes and drops the live session for a host, if any — what
    /// revoking a credential does to the connection it authenticated
    /// (X41), on this replica. Best-effort beyond that: see X44.7.
    pub async fn evict(&self, host_id: Uuid) {
        let removed = self
            .links
            .lock()
            .expect("agent registry poisoned")
            .remove(&host_id);
        if let Some(session) = removed {
            session.disconnect().await;
        }
    }
}
