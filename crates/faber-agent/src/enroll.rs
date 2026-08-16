//! `faber-agent install --token … --api …` — the one-time exchange (X40).
//!
//! Generates this daemon's SSH host keypair locally, trades the bootstrap
//! token for a long-lived connection credential, and writes both to disk.
//! The bootstrap token is never seen again after this call — only the
//! credential this exchange returns is worth anything from here on.

use russh::keys::{Algorithm, PrivateKey, ssh_key::LineEnding};
use serde::{Deserialize, Serialize};

use crate::config::{Config, Scope};

#[derive(Serialize)]
struct ExchangeRequest<'a> {
    token: &'a str,
    host_pubkey: &'a str,
}

#[derive(Deserialize)]
struct ExchangeResponse {
    credential: String,
}

pub async fn install(
    api: &str,
    token: &str,
    scope: Scope,
    start: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Before the exchange consumes the single-use token, because a system
    // install that cannot write `/etc` or talk to the system manager is a
    // failure the operator repairs by re-running with the right authority —
    // and re-running is exactly what a consumed token forbids.
    if scope == Scope::System {
        require_root()?;
    }

    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let host_pubkey = key.public_key().to_openssh()?;
    let host_private_key = key.to_openssh(LineEnding::LF)?.to_string();

    // Ambient HTTP_PROXY/HTTPS_PROXY/NO_PROXY, deliberately: this daemon *is*
    // the caller, running on the caller's own infrastructure, so reading its
    // own environment is the ambient trust decision R11 reserves to whoever
    // owns it (X40) — `reqwest::Client::new()` honors those by default.
    let client = reqwest::Client::new();
    let url = format!("{}/api/agent/enroll", api.trim_end_matches('/'));
    let response = client
        .post(&url)
        .json(&ExchangeRequest {
            token,
            host_pubkey: &host_pubkey,
        })
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("enrollment exchange failed ({status}): {body}").into());
    }

    let exchanged: ExchangeResponse = response.json().await?;

    let config = Config {
        api: api.to_owned(),
        credential: exchanged.credential,
        host_private_key,
    };
    config.save(scope)?;

    println!(
        "enrolled. host key fingerprint: {}",
        russh::keys::ssh_key::PublicKey::from_openssh(&host_pubkey)?
            .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
    );
    println!(
        "config written to {}",
        crate::config::config_dir(scope)?
            .join("config.json")
            .display()
    );

    // After the exchange, never before: the bootstrap token is single-use, so
    // a unit written first and an exchange that then fails would leave a
    // service pointed at a credential that does not exist and a token that
    // cannot be redeemed again.
    crate::service::install_unit(scope, start)?;

    Ok(())
}

/// Refuses a system install that is not running as root.
///
/// An agent's privilege is fixed at install and never negotiated, so this is
/// the one moment the question can be asked at all. Failing here says what
/// is wrong; proceeding would write `/etc` successfully on some machines,
/// fail confusingly on others, and in either case produce a daemon whose
/// authority is not the one the operator asked for.
#[cfg(unix)]
fn require_root() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: `geteuid` takes no arguments, touches no memory, and cannot
    // fail.
    if unsafe { libc::geteuid() } != 0 {
        return Err(
            "--system installs a system unit and writes /etc/faber-agent; run it as root"
                .to_owned()
                .into(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_root() -> Result<(), Box<dyn std::error::Error>> {
    Err("--system is only meaningful on a systemd machine"
        .to_owned()
        .into())
}
