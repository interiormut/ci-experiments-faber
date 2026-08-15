//! Getting the daemon onto a machine faber cannot reach.
//!
//! Enrollment already answers "prove this daemon is ours." This answers the
//! step before it: there has to *be* a daemon on the target, and the person
//! setting it up should not have to build one. So the API serves both halves
//! of a one-liner — a shell script that knows where to fetch from, and the
//! binaries it fetches.
//!
//! Neither route authenticates, deliberately. The script and the binary are
//! the same bytes for everyone; the secret in the flow is the bootstrap
//! token, which the operator pastes into the command and which the
//! enrollment exchange checks. Gating the download on a session would mean a
//! machine with no browser and no faber account could not run the command
//! that exists precisely for machines like that.
//!
//! The binaries are baked into the API image at build time, which makes the
//! served daemon and the running server the same release by construction —
//! a daemon can be stale relative to the server only by not being
//! reinstalled, never by being downloaded.

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio_util::io::ReaderStream;

use crate::{
    error::{ApiResult, AppError},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agent/install.sh", get(script))
        .route("/api/agent/binary/{arch}", get(binary))
}

/// The installer script, with faber's own URL substituted in at serve time.
///
/// Templated rather than static because the script's whole job is to reach
/// back here, and only the running process knows what "here" is from
/// outside.
const INSTALL_SH: &str = include_str!("install.sh");

/// Where the one-liner puts the binary. Under `$HOME` on purpose: the daemon
/// installs as a user service running with the privileges of whoever ran the
/// command, so the install writes nothing that would need more rights than
/// that account already has, and needs no `sudo` anywhere.
fn render_script(base: &str) -> String {
    INSTALL_SH.replace("@FABER_API@", base)
}

async fn script(State(state): State<AppState>) -> ApiResult<Response> {
    let base = public_url(&state)?;
    let body = render_script(&base);

    Ok((
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            // A script piped straight into a shell should never be a cached
            // copy of an older release than the binaries beside it.
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}

async fn binary(State(state): State<AppState>, Path(arch): Path<String>) -> ApiResult<Response> {
    // The arch reaches this straight from `uname -m` on an unknown machine
    // and is about to become part of a path. Nothing outside this set is a
    // real architecture name, and rejecting rather than sanitizing keeps the
    // check obvious.
    if !is_architecture_name(&arch) {
        return Err(AppError::BadRequest("not an architecture name".into()));
    }

    let dir = &state.config.agent_binary_dir;
    let mut path = dir.join(format!("faber-agent-{arch}"));

    // A dev checkout has `cargo build`'s plain `faber-agent` and no
    // per-arch copies. Accepting it only for the server's own architecture
    // keeps that convenience from ever serving an x86_64 binary to an arm
    // host, which fails as `exec format error` well after the download looks
    // like it worked.
    if !path.exists() && arch == std::env::consts::ARCH {
        let unsuffixed = dir.join("faber-agent");
        if unsuffixed.exists() {
            path = unsuffixed;
        }
    }

    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(%arch, path = %path.display(), "no agent binary for this architecture");
            return Err(AppError::NotFound);
        }
        Err(error) => {
            tracing::error!(%arch, path = %path.display(), %error, "agent binary unreadable");
            return Err(AppError::ServiceUnavailable(
                "the agent binary for this architecture is present but unreadable".into(),
            ));
        }
    };

    let length = file.metadata().await.ok().map(|meta| meta.len());
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"faber-agent\"",
        );
    if let Some(length) = length {
        response = response.header(header::CONTENT_LENGTH, length);
    }

    Ok(response
        .body(Body::from_stream(ReaderStream::new(file)))
        .expect("agent binary response is well-formed"))
}

fn is_architecture_name(arch: &str) -> bool {
    !arch.is_empty()
        && arch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Faber's externally reachable base URL, or an error explaining what to set.
///
/// Every caller of this is building something a *remote* machine will use,
/// so guessing is worse than failing: a wrong base URL produces an install
/// command that runs, downloads nothing, and reports a network error on a
/// machine the operator has to go and read logs on.
pub fn public_url(state: &AppState) -> Result<String, AppError> {
    state.config.public_url.clone().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "FABER_PUBLIC_URL is not set, so faber cannot tell a remote machine where to reach it"
                .into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_served_script_carries_no_placeholder() {
        let body = render_script("https://faber.example.com");
        // A placeholder that survives templating reaches the target as a
        // literal `@FABER_API@` in a download URL, which fails on the far
        // machine rather than here.
        assert!(!body.contains("@FABER_API@"));
        assert!(body.contains("API=\"https://faber.example.com\""));
        assert!(body.contains("$API/api/agent/binary/$ARCH"));
    }

    #[test]
    fn an_architecture_may_not_walk_out_of_the_binary_directory() {
        assert!(is_architecture_name("x86_64"));
        assert!(is_architecture_name("aarch64"));
        assert!(!is_architecture_name(""));
        assert!(!is_architecture_name("../../etc/passwd"));
        assert!(!is_architecture_name("x86_64/../.."));
    }
}
