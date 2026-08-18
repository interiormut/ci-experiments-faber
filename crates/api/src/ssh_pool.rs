//! Process-wide SSH sessions shared by environment binds and live previews.
//!
//! The cache identity includes every configuration value that changes who or
//! what is reached. Credential rotation creates a new credential id, and a
//! changed address or pinned host key therefore cannot reuse an old session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use environment::ssh::{HostKey, SshCredential, SshSession};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{
    environments::{fault, record_host_key},
    error::ApiResult,
    models::host::Host,
    resolve::{ResolvedHostKey, resolve_host_key},
    state::AppState,
};

struct Entry {
    credential_id: Uuid,
    address: String,
    user: String,
    fingerprint: String,
    session: Arc<SshSession>,
}

impl Entry {
    fn matches(&self, resolved: &ResolvedHostKey) -> bool {
        self.credential_id == resolved.credential_id
            && self.address == resolved.address
            && self.user == resolved.user
            && resolved
                .host_key
                .as_ref()
                .is_none_or(|expected| expected == &self.fingerprint)
            && !self.session.is_closed()
    }
}

#[derive(Default)]
pub struct SshSessionPool {
    entries: Mutex<HashMap<Uuid, Entry>>,
    gates: Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>,
}

impl SshSessionPool {
    /// Returns the one live session for this exact host configuration.
    /// Establishment is single-flight per host; unrelated hosts connect in
    /// parallel and no synchronous lock is held over network I/O.
    pub async fn get(
        &self,
        state: &AppState,
        user_id: Uuid,
        host: &Host,
    ) -> ApiResult<Arc<SshSession>> {
        let resolved = resolve_host_key(state, user_id, host).await?;
        if let Some(session) = self.cached(host.id, &resolved) {
            return Ok(session);
        }

        let gate = {
            let mut gates = self.gates.lock().expect("ssh pool gates poisoned");
            Arc::clone(
                gates
                    .entry(host.id)
                    .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
            )
        };
        let _guard = gate.lock().await;
        if let Some(session) = self.cached(host.id, &resolved) {
            return Ok(session);
        }

        let credential = SshCredential {
            user: resolved.user.clone(),
            private_key: resolved.private_key.clone(),
            passphrase: None,
        };
        let expectation = resolved
            .host_key
            .clone()
            .map(HostKey::Verify)
            .unwrap_or(HostKey::AcceptNew);
        let (session, fingerprint) =
            SshSession::connect(&resolved.address, &credential, expectation)
                .await
                .map_err(fault)?;
        record_host_key(state, host, &fingerprint).await?;
        let session = Arc::new(session);

        let previous = self.entries.lock().expect("ssh pool poisoned").insert(
            host.id,
            Entry {
                credential_id: resolved.credential_id,
                address: resolved.address,
                user: resolved.user,
                fingerprint,
                session: Arc::clone(&session),
            },
        );
        if let Some(previous) = previous {
            previous.session.disconnect().await;
        }
        Ok(session)
    }

    fn cached(&self, host_id: Uuid, resolved: &ResolvedHostKey) -> Option<Arc<SshSession>> {
        let mut entries = self.entries.lock().expect("ssh pool poisoned");
        match entries.get(&host_id) {
            Some(entry) if entry.matches(resolved) => Some(Arc::clone(&entry.session)),
            Some(entry) if entry.session.is_closed() => {
                entries.remove(&host_id);
                None
            }
            _ => None,
        }
    }
}
