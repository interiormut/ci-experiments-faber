//! Reaching a user's machine from a request handler.
//!
//! Everything the `environment` crate does starts from something dialled, and
//! what to dial is always configuration passed in — never the process's own.
//! This module is the one place that configuration is assembled, and it
//! assembles it from the caller's own rows: `host.docker_endpoint`,
//! `host.ssh_address`, and the credential `host.ssh_key_ref` names. Faber runs
//! as a multi-user service, so `DOCKER_HOST`, `~/.ssh`, an agent socket, and a
//! docker context describe the operator and would send one user's work out
//! under another's identity. None of them is read here, and none should be.
//!
//! Two things are deliberately not cached: the SSH session and the daemon
//! connection. A `Daemon` opens a fresh connection per call by design, and a
//! session held between requests is a session that has to be invalidated when
//! a user rotates a key — reconnecting costs one round trip and owes nobody an
//! invalidation story.

use std::sync::Arc;

use diesel::{ExpressionMethods, QueryDsl};
use diesel_async::RunQueryDsl;
use environment::docker::{Daemon, LocalSocket};
use environment::ssh::forward::DOCKER_SOCKET;
use environment::ssh::{HostKey, SshCredential, SshForwarded, SshSession};
use environment::{Denial, Fault};
use uuid::Uuid;

use crate::{
    error::{ApiResult, AppError},
    models::host::{ExecMode, Host, Transport},
    resolve::resolve_host_key,
    schema::host,
    state::AppState,
};

/// Opens a path to the container daemon a host runs.
///
/// Refuses a host that is not in docker mode: `exec_mode` is the user's
/// statement of what faber does once it has reached the machine, and treating
/// a `direct` host as a docker one because it happens to have an endpoint set
/// would override that statement with a guess.
pub async fn reach_daemon(
    state: &AppState,
    user_id: Uuid,
    host: &Host,
) -> ApiResult<Arc<dyn Daemon>> {
    if host.exec_mode != ExecMode::Docker.as_str() {
        return Err(AppError::BadRequest(
            "this host's exec_mode is not 'docker', so it runs no container daemon".into(),
        ));
    }
    if host.disabled_at.is_some() {
        return Err(AppError::BadRequest("this host is disabled".into()));
    }

    if host.transport == Transport::Ssh.as_str() {
        let resolved = resolve_host_key(state, user_id, host).await?;
        let credential = SshCredential {
            user: resolved.user,
            private_key: resolved.private_key,
            passphrase: None,
        };
        // First contact accepts what answers and records it; everything after
        // verifies against what was recorded. There is no third option — a
        // service holding many users' keys cannot afford one.
        let expectation = match &resolved.host_key {
            Some(fingerprint) => HostKey::Verify(fingerprint.clone()),
            None => HostKey::AcceptNew,
        };

        let (session, fingerprint) =
            SshSession::connect(&resolved.address, &credential, expectation)
                .await
                .map_err(fault)?;

        // The socket path is on the *far* side, so a `tcp://` endpoint is not
        // a thing this forward can dial: `direct-streamlocal` opens a unix
        // socket, and asking a user to expose their docker socket to the
        // network instead is a bad trade made on their behalf.
        let socket = match host.docker_endpoint.as_deref() {
            None => DOCKER_SOCKET.to_owned(),
            Some(endpoint) if endpoint.starts_with("unix://") => {
                endpoint.trim_start_matches("unix://").to_owned()
            }
            Some(endpoint) if endpoint.starts_with('/') => endpoint.to_owned(),
            Some(endpoint) => {
                return Err(AppError::BadRequest(format!(
                    "`{endpoint}` cannot be reached over ssh; \
                     name the daemon's socket path on the far side"
                )));
            }
        };

        record_host_key(state, host, &fingerprint).await?;

        return Ok(Arc::new(SshForwarded::new(Arc::new(session), socket)));
    }

    let endpoint = host.docker_endpoint.as_deref().ok_or_else(|| {
        AppError::BadRequest(
            "this host has no docker_endpoint; a daemon faber was not told about \
             is one it should not be talking to"
                .into(),
        )
    })?;

    Ok(Arc::new(LocalSocket::new(endpoint).map_err(fault)?))
}

/// Stores the fingerprint a first connection saw.
///
/// Only when the column is still null, and never on a mismatch: a host that
/// presents a different key is refused by the verifier above and never reaches
/// here, which is what keeps this from being a trust-on-every-use.
async fn record_host_key(state: &AppState, host: &Host, fingerprint: &str) -> ApiResult<()> {
    if host.ssh_host_key.is_some() || fingerprint.is_empty() {
        return Ok(());
    }

    let mut conn = state.db.get().await?;
    diesel::update(
        host::table
            .filter(host::id.eq(host.id))
            .filter(host::ssh_host_key.is_null()),
    )
    .set(host::ssh_host_key.eq(fingerprint))
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "environments.record_host_key"))?;

    Ok(())
}

/// Carries a [`Fault`] across as the status that matches its class.
///
/// The two classes are the whole point of the type: a denial is about the
/// request and the caller repairs it, and an unreachable machine is about the
/// world and the caller retries. Flattening both to 500 would tell a user with
/// a typo in an endpoint that faber is broken.
pub fn fault(fault: Fault) -> AppError {
    match fault {
        Fault::Denied(Denial::NotFound { path }) => {
            AppError::BadRequest(format!("`{path}` was not found on that machine"))
        }
        Fault::Denied(denial) => AppError::BadRequest(denial.to_string()),
        Fault::Unreachable(reason) => AppError::ServiceUnavailable(reason),
    }
}
