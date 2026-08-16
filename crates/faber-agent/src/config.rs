//! What survives a restart: the daemon's own identity and where to dial.
//!
//! Written once at `install` and read at every `run`. There is nothing to
//! reconcile against a server-side copy — the credential and host keypair
//! *are* the identity; losing this file means reinstalling, not resyncing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Faber's base URL, e.g. `https://faber.example.com`.
    pub api: String,
    /// The long-lived connection credential from the enrollment exchange
    /// (X41) — presented as `Authorization: Bearer` on every reconnect.
    pub credential: String,
    /// This daemon's SSH host private key, OpenSSH PEM. Generated once at
    /// install; Faber pinned the public half at that same exchange, so this
    /// can never be regenerated without re-enrolling.
    pub host_private_key: String,
}

/// Whose machine this daemon is, decided once at `install` and stated again
/// on every `run`.
///
/// Not detected, and never negotiated: an agent's privilege is fixed at
/// install and there is no protocol for changing it. A user install yields a
/// `systemctl --user` unit with that account's authority; a system install
/// yields a system unit with whatever provisioning gave it. The scope is a
/// flag on both commands rather than an environment variable or a uid test
/// so that the unit says out loud which daemon it is starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
}

/// Where a system install keeps its identity. `/etc` rather than
/// `/var/lib`: this is configuration written once and read at every start,
/// and it holds a private key — the same thing `/etc/ssh` holds, in the same
/// place, for the same reason.
const SYSTEM_CONFIG_DIR: &str = "/etc/faber-agent";

/// `$XDG_CONFIG_HOME/faber-agent`, falling back to `~/.config/faber-agent` —
/// a user-service install (X40's resolved run-as-user decision) has no
/// business writing outside its own account's config directory. A system
/// install writes under `/etc` instead, because there is no account whose
/// directory would be the right one.
pub fn config_dir(scope: Scope) -> std::io::Result<PathBuf> {
    if scope == Scope::System {
        return Ok(PathBuf::from(SYSTEM_CONFIG_DIR));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("faber-agent"));
    }
    let home = std::env::var("HOME").map_err(|_| {
        std::io::Error::other("neither XDG_CONFIG_HOME nor HOME is set; cannot place config")
    })?;
    Ok(PathBuf::from(home).join(".config").join("faber-agent"))
}

fn config_path(scope: Scope) -> std::io::Result<PathBuf> {
    Ok(config_dir(scope)?.join("config.json"))
}

impl Config {
    pub fn load(scope: Scope) -> std::io::Result<Self> {
        let body = std::fs::read_to_string(config_path(scope)?)?;
        serde_json::from_str(&body)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    /// Writes the config with `0600` permissions — this file holds the
    /// credential that authenticates this daemon to Faber (R15) and the
    /// private half of the key Faber pinned at enrollment (X39). Either one
    /// leaking is the same class of compromise as an SSH private key
    /// leaking, because that is exactly what one of them is.
    pub fn save(&self, scope: Scope) -> std::io::Result<()> {
        let dir = config_dir(scope)?;
        std::fs::create_dir_all(&dir)?;
        let path = config_path(scope)?;
        let body = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        std::fs::write(&path, body)?;
        set_owner_only(&path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_owner_only(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}
