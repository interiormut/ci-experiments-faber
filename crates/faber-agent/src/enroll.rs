//! `faber-agent install --token … --api …` — the one-time exchange (X40).
//!
//! Generates this daemon's SSH host keypair locally, trades the bootstrap
//! token for a long-lived connection credential, and writes both to disk.
//! The bootstrap token is never seen again after this call — only the
//! credential this exchange returns is worth anything from here on.

use russh::keys::{Algorithm, PrivateKey, ssh_key::LineEnding};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Serialize)]
struct ExchangeRequest<'a> {
    token: &'a str,
    host_pubkey: &'a str,
}

#[derive(Deserialize)]
struct ExchangeResponse {
    credential: String,
}

pub async fn install(api: &str, token: &str) -> Result<(), Box<dyn std::error::Error>> {
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
    config.save()?;

    println!(
        "enrolled. host key fingerprint: {}",
        russh::keys::ssh_key::PublicKey::from_openssh(&host_pubkey)?
            .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
    );
    println!(
        "config written to {}",
        crate::config::config_dir()?.join("config.json").display()
    );

    Ok(())
}
