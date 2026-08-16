//! Machine reads Faber cannot make from where it runs.
//!
//! Everything else this daemon serves is shaped like a process or a file:
//! `exec`, sftp, a forwarded socket. Three things Faber needs from a host it
//! operates are shaped like neither — `statvfs` on the tenant filesystem,
//! `MemAvailable` from `/proc/meminfo`, and a cgroup counter — and each of
//! them is a syscall or a procfs read whose result has a type. Running
//! `stat -f` and parsing its output would put a coreutils version between
//! Faber and a number the kernel already hands back in a struct.
//!
//! So: one SSH subsystem, one JSON request per channel, one JSON response,
//! close. A subsystem rather than a sentinel command string, because
//! `exec_request` on this daemon runs whatever a user's session asks of it —
//! a magic argv would be both forgeable and able to shadow a real command.
//!
//! **This grants nothing `exec` does not.** Anything reachable here is
//! readable by a shell on the same connection; the point is the shape of the
//! answer, not the authority behind it.
//!
//! The wire shapes are duplicated in `environment::ssh::probe`, deliberately
//! and in the same spirit as this crate carrying the server half of an SSH
//! implementation whose client half lives there. Both ends are built from
//! one release — the API serves the binary it was built beside — so there is
//! no version to negotiate, and a shared crate would pull `environment` into
//! a daemon that has no other use for it.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// The subsystem name Faber requests. Namespaced so it cannot collide with
/// `sftp` or with anything an sshd would offer.
pub const SUBSYSTEM: &str = "faber-probe";

/// A request is one line and never more: this is a read, and a read that
/// needs framing is a protocol rather than a question.
const MAX_REQUEST_BYTES: u64 = 8 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    /// `statvfs` on the filesystem holding a path.
    Capacity { path: String },
    /// `MemTotal` and `MemAvailable`, in bytes.
    Memory,
    /// Whole numbers read out of files, one per path — cgroup counters, in
    /// practice. `null` for a path that could not be read or did not hold
    /// one, which is what separates "the machine says zero" from "the
    /// machine did not answer": a link failure never reaches here at all.
    Counters { paths: Vec<String> },
}

/// Every answer carries either the fields it was asked for or `error`, never
/// both. A transport failure is not representable here on purpose — it is
/// the absence of a response, and the caller distinguishes the two.
#[derive(Debug, Default, Serialize)]
struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<Vec<Option<u64>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn failed(reason: impl std::fmt::Display) -> Self {
        Response {
            error: Some(reason.to_string()),
            ..Default::default()
        }
    }
}

/// Serves one request on one channel and returns.
///
/// The channel is the framing: Faber opens one, asks one question, reads the
/// answer, and closes. Keeping a request/response loop alive on it would buy
/// nothing — these calls are minutes apart at most — and would need a
/// framing story for interleaved answers.
pub async fn serve<S>(stream: S)
where
    S: tokio::io::AsyncRead + AsyncWrite + Unpin + Send,
{
    let (reader, mut writer) = tokio::io::split(stream);
    // Capped at the reader, not checked afterwards: a peer that never sends
    // a newline would otherwise buffer until something else stopped it.
    let mut reader = BufReader::new(reader.take(MAX_REQUEST_BYTES));
    let mut line = String::new();

    let response = match reader.read_line(&mut line).await {
        Ok(0) => return, // Nothing asked; nothing to answer.
        Ok(_) => match serde_json::from_str::<Request>(&line) {
            Ok(request) => answer(request).await,
            Err(error) => Response::failed(format!("unreadable request: {error}")),
        },
        Err(error) => Response::failed(format!("could not read the request: {error}")),
    };

    let mut body =
        serde_json::to_vec(&response).unwrap_or_else(|_| b"{\"error\":\"unencodable\"}".to_vec());
    body.push(b'\n');

    let _ = writer.write_all(&body).await;
    let _ = writer.flush().await;
    let _ = writer.shutdown().await;
}

async fn answer(request: Request) -> Response {
    match request {
        Request::Capacity { path } => match capacity(&path) {
            Ok((total, available)) => Response {
                total_bytes: Some(total),
                available_bytes: Some(available),
                ..Default::default()
            },
            Err(error) => Response::failed(error),
        },
        Request::Memory => match memory().await {
            Ok((total, available)) => Response {
                total_bytes: Some(total),
                available_bytes: Some(available),
                ..Default::default()
            },
            Err(error) => Response::failed(error),
        },
        Request::Counters { paths } => {
            let mut values = Vec::with_capacity(paths.len());
            for path in &paths {
                values.push(counter(path).await);
            }
            Response {
                values: Some(values),
                ..Default::default()
            }
        }
    }
}

/// Bytes on the filesystem holding a path: its total size and what is free
/// to an unprivileged writer.
#[cfg(unix)]
fn capacity(path: &str) -> Result<(u64, u64), String> {
    use std::ffi::CString;

    let raw = CString::new(path).map_err(|_| format!("{path} is not a usable path"))?;

    // SAFETY: `stats` is written by the call and only read once it reports
    // success; `raw` outlives the call.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(raw.as_ptr(), &mut stats) } != 0 {
        return Err(format!(
            "could not stat the filesystem at {path}: {}",
            std::io::Error::last_os_error()
        ));
    }

    let block = stats.f_frsize as u64;
    Ok((
        block * stats.f_blocks as u64,
        // `f_bavail`, not `f_bfree`: the reserve root can still write into
        // is not space a tenant will ever see.
        block * stats.f_bavail as u64,
    ))
}

#[cfg(not(unix))]
fn capacity(_path: &str) -> Result<(u64, u64), String> {
    Err("this machine has no statvfs".to_owned())
}

async fn memory() -> Result<(u64, u64), String> {
    let text = tokio::fs::read_to_string("/proc/meminfo")
        .await
        .map_err(|error| format!("could not read /proc/meminfo: {error}"))?;

    let field = |name: &str| -> Option<u64> {
        text.lines()
            .find(|line| line.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
            .map(|kilobytes| kilobytes * 1024)
    };

    let available = field("MemAvailable:")
        .ok_or_else(|| "/proc/meminfo did not report MemAvailable".to_owned())?;
    Ok((field("MemTotal:").unwrap_or(0), available))
}

async fn counter(path: &str) -> Option<u64> {
    tokio::fs::read_to_string(path)
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_counter_that_cannot_be_read_is_absent_rather_than_zero() {
        // The whole reason the counters op returns `Option` per path: a
        // controller that is not enabled and a controller reporting zero are
        // different facts, and rendering them the same is how a tenant gets
        // told they are using no memory.
        assert_eq!(counter("/proc/does-not-exist").await, None);
    }

    #[tokio::test]
    async fn an_unreadable_request_is_answered_rather_than_dropped() {
        // A caller waiting on a response it will never get is worse than one
        // told what was wrong: the link stays open and the read blocks until
        // its own timeout.
        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(serve(server));

        let (reader, mut writer) = tokio::io::split(client);
        writer.write_all(b"not json\n").await.unwrap();
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).await.unwrap();
        assert!(line.contains("unreadable request"));
    }

    #[tokio::test]
    async fn memory_is_read_out_of_procfs_when_there_is_one() {
        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(serve(server));

        let (reader, mut writer) = tokio::io::split(client);
        writer.write_all(b"{\"op\":\"memory\"}\n").await.unwrap();
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).await.unwrap();

        // Linux answers; anything else says why rather than inventing a
        // number, and both are a well-formed response.
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(parsed.get("available_bytes").is_some() || parsed.get("error").is_some());
    }
}
