//! Asking a machine for a number, over a session already open to it.
//!
//! The other half of `faber-agent`'s `probe` module. Three reads — a
//! filesystem's size, a machine's free memory, and a cgroup counter — that
//! everywhere else in this crate would be a syscall, and that from a process
//! which is not on the machine cannot be. They are not commands: `stat -f`
//! and `cat /proc/meminfo` would work, and would put a coreutils version and
//! an output format between faber and values the kernel returns in a struct.
//!
//! One channel per question, opened on the existing session: no new
//! connection, no dial, nothing to authenticate. A daemon too old to know
//! the subsystem refuses the request, which surfaces as `Unreachable`
//! naming the version skew rather than as a parse failure on empty output.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::fault::Fault;
use crate::ssh::SshSession;
use crate::tenancy::{Capacity, Memory, Reads};

/// Must match `faber-agent`'s `probe::SUBSYSTEM`.
const SUBSYSTEM: &str = "faber-probe";

/// How long one read may take. Generous for a syscall and a round trip, and
/// short enough that an admission check does not wait out a wedged link —
/// which is the whole reason these reads happen before the admission lock
/// rather than inside it.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// A response is one line of JSON; anything larger is a peer that is not
/// speaking this protocol.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request<'a> {
    Capacity { path: &'a str },
    Memory,
    Counters { paths: &'a [String] },
}

#[derive(Deserialize)]
struct Response {
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
    values: Option<Vec<Option<u64>>>,
    /// The machine answered and said no. Distinct from the transport
    /// failing, which is an `Err` and never reaches here.
    error: Option<String>,
}

/// Machine reads served by the daemon on the far end of an agent link.
pub struct AgentReads {
    session: Arc<SshSession>,
}

impl AgentReads {
    pub fn new(session: Arc<SshSession>) -> Self {
        AgentReads { session }
    }

    async fn ask(&self, request: &Request<'_>) -> Result<Response, Fault> {
        let body = serde_json::to_vec(request).map_err(|error| {
            Fault::Unreachable(format!("could not encode a machine read: {error}"))
        })?;

        match tokio::time::timeout(READ_TIMEOUT, self.exchange(body)).await {
            Ok(result) => result,
            Err(_) => Err(Fault::Unreachable(format!(
                "the agent did not answer a machine read within {READ_TIMEOUT:?}"
            ))),
        }
    }

    async fn exchange(&self, mut body: Vec<u8>) -> Result<Response, Fault> {
        let channel = self
            .session
            .handle()
            .channel_open_session()
            .await
            .map_err(|error| Fault::Unreachable(format!("could not open a channel: {error}")))?;
        channel
            .request_subsystem(true, SUBSYSTEM)
            .await
            .map_err(|error| {
                Fault::Unreachable(format!(
                    "the agent refused the '{SUBSYSTEM}' subsystem ({error}); \
                     it is likely older than this faber"
                ))
            })?;

        let stream = channel.into_stream();
        let (reader, mut writer) = tokio::io::split(stream);
        body.push(b'\n');
        writer.write_all(&body).await.map_err(|error| {
            Fault::Unreachable(format!("could not send a machine read: {error}"))
        })?;
        writer.flush().await.map_err(|error| {
            Fault::Unreachable(format!("could not send a machine read: {error}"))
        })?;

        let mut line = String::new();
        BufReader::new(reader.take(MAX_RESPONSE_BYTES))
            .read_line(&mut line)
            .await
            .map_err(|error| Fault::Unreachable(format!("could not read the answer: {error}")))?;

        if line.trim().is_empty() {
            return Err(Fault::Unreachable(
                "the agent closed a machine read without answering".to_owned(),
            ));
        }

        serde_json::from_str(&line).map_err(|error| {
            Fault::Unreachable(format!("unreadable answer from the agent: {error}"))
        })
    }
}

/// The two-field answer `capacity` and `memory` share, checked once.
fn sized(response: Response, what: &str) -> Result<(u64, u64), Fault> {
    if let Some(error) = response.error {
        return Err(Fault::Unreachable(format!("{what}: {error}")));
    }
    match (response.total_bytes, response.available_bytes) {
        (Some(total), Some(available)) => Ok((total, available)),
        _ => Err(Fault::Unreachable(format!(
            "the agent's answer for {what} was missing its numbers"
        ))),
    }
}

#[async_trait]
impl Reads for AgentReads {
    async fn capacity(&self, path: &str) -> Result<Capacity, Fault> {
        let response = self.ask(&Request::Capacity { path }).await?;
        let (total_bytes, available_bytes) = sized(response, "the filesystem")?;
        Ok(Capacity {
            total_bytes,
            available_bytes,
        })
    }

    async fn memory(&self) -> Result<Memory, Fault> {
        let response = self.ask(&Request::Memory).await?;
        let (total_bytes, available_bytes) = sized(response, "memory")?;
        Ok(Memory {
            total_bytes,
            available_bytes,
        })
    }

    async fn counters(&self, paths: &[String]) -> Result<Vec<Option<u64>>, Fault> {
        let response = self.ask(&Request::Counters { paths }).await?;
        if let Some(error) = response.error {
            return Err(Fault::Unreachable(format!("counters: {error}")));
        }
        // Short is not a partial answer to be padded: a daemon that sent a
        // different number of values than it was asked for is one this
        // faber does not understand, and guessing which counter is missing
        // would attribute one tenant's memory to another's line.
        match response.values {
            Some(values) if values.len() == paths.len() => Ok(values),
            _ => Err(Fault::Unreachable(
                "the agent answered a counter read with the wrong number of values".to_owned(),
            )),
        }
    }
}
