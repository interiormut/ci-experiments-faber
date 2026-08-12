//! Running a command on the far side of an SSH session.
//!
//! SSH runs one command *string* through the login shell, not an argv, so the
//! request has to be reassembled into a script. Three things ride along with
//! it, and each is here because SSH offers no other way:
//!
//! - **The working directory**, as a `cd`, because there is no field for it.
//! - **The environment**, as assignments, because `AcceptEnv` is empty on
//!   almost every sshd and a rejected `env` request is silent.
//! - **The pid**, recorded to a file, because killing the local end of a
//!   channel does not kill the remote process and `SSH_MSG_CHANNEL_REQUEST
//!   signal` is ignored by OpenSSH's server. A signal is a second channel
//!   running `kill`.
//!
//! The command itself is still passed as arguments rather than interpolated,
//! so the agent's shell string is quoted exactly once and never re-parsed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use russh::ChannelMsg;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::exec::{Outcome, Signal};
use crate::fault::{Denial, Fault};
use crate::spawn::{Proc, Run, Sink, Source, Spawn};
use crate::ssh::{SshSession, quote};

/// Where a command's pid is left so a later signal can find it. Same
/// unresolved cleanup question as spilled output.
const PROC_DIR: &str = ".faber/proc";

pub struct SshSpawn {
    session: Arc<SshSession>,
    root: String,
    nonce: u64,
    next: AtomicU64,
}

impl SshSpawn {
    pub fn new(session: Arc<SshSession>, root: impl Into<String>) -> Self {
        SshSpawn {
            session,
            root: root.into(),
            nonce: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64),
            next: AtomicU64::new(1),
        }
    }

    fn pid_path(&self) -> String {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let root = self.root.trim_end_matches('/');
        format!("{root}/{PROC_DIR}/{:x}-{id}", self.nonce)
    }

    /// Runs a command string and waits for it, discarding its output.
    ///
    /// Used only for signalling, where the exit code is the whole answer.
    async fn run_bare(&self, command: String) -> Result<(), Fault> {
        let mut channel = self
            .session
            .handle()
            .channel_open_session()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        channel
            .exec(true, command)
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;

        while let Some(message) = channel.wait().await {
            if matches!(message, ChannelMsg::Close | ChannelMsg::ExitStatus { .. }) {
                break;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Spawn for SshSpawn {
    async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault> {
        if run.pty {
            return Err(Fault::Denied(Denial::MissingCapability(
                crate::manifest::Capability::Pty,
            )));
        }
        let Some((program, arguments)) = run.argv.split_first() else {
            return Err(Fault::Denied(Denial::Malformed {
                what: "command".into(),
                reason: "an empty argv names no program".to_owned(),
            }));
        };

        let pid_path = self.pid_path();
        let mut script = format!(
            "mkdir -p {} 2>/dev/null; printf %s $$ > {} 2>/dev/null; cd {} || exit 127; ",
            quote(parent_of(&pid_path).as_str()),
            quote(&pid_path),
            quote(&run.cwd),
        );
        for (key, value) in &run.env {
            // Assignments rather than `env`, so nothing depends on the far
            // side carrying coreutils.
            script.push_str(&format!("export {key}={}; ", quote(value)));
        }
        // `exec "$0" "$@"` with the command supplied as arguments: the agent's
        // string is quoted once, by us, and the far side's shell never sees it
        // as syntax.
        script.push_str("exec \"$0\" \"$@\"");

        let mut command = format!("/bin/sh -c {} {}", quote(&script), quote(program));
        for argument in arguments {
            command.push(' ');
            command.push_str(&quote(argument));
        }

        let channel = self
            .session
            .handle()
            .channel_open_session()
            .await
            .map_err(|error| Fault::Unreachable(format!("could not open a channel: {error}")))?;
        channel
            .exec(true, command)
            .await
            .map_err(|error| Fault::Unreachable(format!("could not start the command: {error}")))?;

        let stdin = channel.make_writer();
        let (mut reader, _writer) = channel.split();

        let (mut out, out_reader) = tokio::io::duplex(64 * 1024);
        let (mut err, err_reader) = tokio::io::duplex(64 * 1024);
        let outcome = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&outcome);

        tokio::spawn(async move {
            while let Some(message) = reader.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        if out.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    // Extended data type 1 is stderr; there are no others in
                    // practice, and anything else is not output.
                    ChannelMsg::ExtendedData { data, ext: 1 } => {
                        if err.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        *recorded.lock().await = Some(Outcome::Completed {
                            code: exit_status as i32,
                        });
                    }
                    ChannelMsg::Close | ChannelMsg::Eof => break,
                    _ => {}
                }
            }
            // A channel that closed without a status did not report one; the
            // command still ended, and saying so beats hanging a reader.
            let mut slot = recorded.lock().await;
            if slot.is_none() {
                *slot = Some(Outcome::Completed { code: -1 });
            }
        });

        Ok(Box::new(SshProc {
            session: Arc::clone(&self.session),
            pid_path,
            stdin: Some(Box::new(stdin) as Sink),
            stdout: Some(Box::new(out_reader) as Source),
            stderr: Some(Box::new(err_reader) as Source),
            outcome,
        }))
    }
}

struct SshProc {
    session: Arc<SshSession>,
    pid_path: String,
    stdin: Option<Sink>,
    stdout: Option<Source>,
    stderr: Option<Source>,
    /// Filled by the pump task when the channel reports a status.
    outcome: Arc<Mutex<Option<Outcome>>>,
}

#[async_trait]
impl Proc for SshProc {
    fn stdin(&mut self) -> Option<Sink> {
        self.stdin.take()
    }

    fn stdout(&mut self) -> Option<Source> {
        self.stdout.take()
    }

    fn stderr(&mut self) -> Option<Source> {
        self.stderr.take()
    }

    async fn wait(&mut self) -> Result<Outcome, Fault> {
        loop {
            if let Some(outcome) = *self.outcome.lock().await {
                return Ok(outcome);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault> {
        Ok(*self.outcome.lock().await)
    }

    async fn signal(&mut self, signal: Signal) -> Result<(), Fault> {
        // OpenSSH's server ignores the protocol's own signal request, so this
        // reads back the pid the command recorded and sends the signal from a
        // shell on the far side. `kill` is a shell builtin; nothing about this
        // depends on what the machine has installed.
        let spawn = SshSpawn::new(Arc::clone(&self.session), "/");
        spawn
            .run_bare(format!(
                "kill -{} \"$(cat {} 2>/dev/null)\" 2>/dev/null",
                signal.number(),
                quote(&self.pid_path),
            ))
            .await
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(at) => path[..at].to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_path_sits_under_the_root() {
        assert_eq!(parent_of("/work/.faber/proc/ab-1"), "/work/.faber/proc");
        assert_eq!(parent_of("/top"), "/");
    }
}
