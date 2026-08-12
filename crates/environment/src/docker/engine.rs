//! The five Engine API calls this crate needs, and nothing else.
//!
//! Pinned to `v1.41` — old enough that any daemon a user is plausibly running
//! speaks it, new enough to carry everything here. An unversioned path would
//! follow whatever the daemon defaults to, which makes Faber's behavior a
//! function of someone else's upgrade schedule.
//!
//! Every call opens its own connection. See [`Daemon`] for why.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::docker::daemon::{Conn, Daemon};
use crate::docker::http::Wire;
use crate::fault::{Denial, Fault};

/// The API version every path is prefixed with.
const API: &str = "/v1.41";

/// What an exec is doing, as the daemon sees it.
pub struct ExecState {
    pub running: bool,
    pub exit_code: Option<i64>,
}

/// Creates an exec and returns its id. Does not start it.
pub async fn exec_create(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    cmd: &[String],
    env: &[(String, String)],
    workdir: &str,
    tty: bool,
) -> Result<String, Fault> {
    let body = json!({
        "AttachStdin": true,
        "AttachStdout": true,
        "AttachStderr": true,
        "Tty": tty,
        "Cmd": cmd,
        "Env": env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "WorkingDir": workdir,
    });

    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "POST",
            &format!("{API}/containers/{}/exec", encode(container)),
            Some(body.to_string().as_bytes()),
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    if !head.ok() {
        return Err(refused(head.status, &payload, container));
    }

    serde_json::from_slice::<Value>(&payload)
        .ok()
        .and_then(|value| value.get("Id")?.as_str().map(str::to_owned))
        .ok_or_else(|| Fault::Unreachable("the daemon created an exec with no id".to_owned()))
}

/// Starts an exec and hijacks the connection.
///
/// What comes back is the raw bidirectional stream: stdin goes up it
/// unframed, and stdout and stderr come down it multiplexed, which is why
/// [`super::spawn`] has to demultiplex.
pub async fn exec_start(daemon: &Arc<dyn Daemon>, exec: &str, tty: bool) -> Result<Conn, Fault> {
    let body = json!({ "Detach": false, "Tty": tty });

    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "POST",
            &format!("{API}/exec/{}/start", encode(exec)),
            Some(body.to_string().as_bytes()),
            true,
        )
        .await?;

    // 101 is the hijack. Some daemons answer 200 and stream instead, which
    // works identically for reading and simply will not carry stdin.
    if head.status != 101 && head.status != 200 {
        let payload = wire.body(&head).await?;
        return Err(refused(head.status, &payload, exec));
    }
    Ok(wire.hijack())
}

pub async fn exec_inspect(daemon: &Arc<dyn Daemon>, exec: &str) -> Result<ExecState, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "GET",
            &format!("{API}/exec/{}/json", encode(exec)),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    if !head.ok() {
        return Err(refused(head.status, &payload, exec));
    }

    let value: Value = serde_json::from_slice(&payload)
        .map_err(|_| Fault::Unreachable("the daemon sent an unreadable exec state".to_owned()))?;
    Ok(ExecState {
        running: value
            .get("Running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        exit_code: value.get("ExitCode").and_then(Value::as_i64),
    })
}

/// Pulls a path out of the container as a tar stream.
pub async fn archive_get(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    path: &str,
) -> Result<Vec<u8>, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "GET",
            &format!(
                "{API}/containers/{}/archive?path={}",
                encode(container),
                encode(path)
            ),
            None,
            false,
        )
        .await?;
    let payload = wire.body(&head).await?;

    match head.status {
        200 => Ok(payload),
        404 => Err(Fault::Denied(Denial::NotFound {
            path: path.to_owned(),
        })),
        status => Err(refused(status, &payload, path)),
    }
}

/// Unpacks a tar into a directory that already exists in the container.
pub async fn archive_put(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    directory: &str,
    tar: &[u8],
) -> Result<(), Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request_tar(
            &format!(
                "{API}/containers/{}/archive?path={}",
                encode(container),
                encode(directory)
            ),
            tar,
        )
        .await?;
    let payload = wire.body(&head).await?;

    match head.status {
        200 => Ok(()),
        404 => Err(Fault::Denied(Denial::NotFound {
            path: directory.to_owned(),
        })),
        status => Err(refused(status, &payload, directory)),
    }
}

/// What the daemon knows about one path, without moving its contents.
///
/// The stat rides in a base64 header rather than the body, which is why this
/// is a `HEAD` and why it is worth having: it answers "does this exist, and is
/// it a directory" in one round trip and without pulling a byte of content.
pub async fn archive_stat(
    daemon: &Arc<dyn Daemon>,
    container: &str,
    path: &str,
) -> Result<Option<Value>, Fault> {
    let mut wire = Wire::new(daemon.connect().await?);
    let head = wire
        .request(
            "HEAD",
            &format!(
                "{API}/containers/{}/archive?path={}",
                encode(container),
                encode(path)
            ),
            None,
            false,
        )
        .await?;

    match head.status {
        200 => {
            let Some(encoded) = head.header("x-docker-container-path-stat") else {
                return Ok(None);
            };
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| Fault::Unreachable("unreadable path stat".to_owned()))?;
            Ok(serde_json::from_slice(&raw).ok())
        }
        404 => Ok(None),
        status => Err(refused(status, &[], path)),
    }
}

/// A daemon's refusal, carried across as the class the agent should act on.
///
/// A 4xx is about the request and a 5xx is about the daemon, which is the same
/// split the agent already knows how to repair.
fn refused(status: u16, payload: &[u8], what: &str) -> Fault {
    let message = serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("the daemon answered {status}"));

    match status {
        404 => Fault::Denied(Denial::NotFound {
            path: what.to_owned(),
        }),
        // A stopped container cannot exec. It is not gone and the request was
        // not malformed, so the honest answer is that it could not be reached.
        409 => Fault::Unreachable(format!("`{what}` is not running: {message}")),
        400..=499 => Fault::Denied(Denial::Malformed {
            what: "docker request".into(),
            reason: message,
        }),
        _ => Fault::Unreachable(message),
    }
}

/// Percent-encodes a query value. Container refs and paths both reach here,
/// and a path with a space in it is not a malformed request.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_a_space_survives_the_query_string() {
        assert_eq!(encode("/a b/c"), "%2Fa%20b%2Fc");
        assert_eq!(encode("plain-name_1.2~"), "plain-name_1.2~");
    }

    #[test]
    fn a_stopped_container_is_unreachable_rather_than_a_denial() {
        let fault = refused(409, br#"{"message":"is not running"}"#, "build");
        assert!(matches!(fault, Fault::Unreachable(_)));
    }

    #[test]
    fn a_missing_path_is_not_found_whatever_the_body_says() {
        let fault = refused(404, b"", "/nope");
        assert!(matches!(fault, Fault::Denied(Denial::NotFound { .. })));
    }
}
