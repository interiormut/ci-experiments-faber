//! Runs a command for `exec_request`/`shell_request` and pumps its output
//! back over the channel — the far side of what `SshSpawn` drives
//! (`crates/environment/src/ssh/spawn.rs`).
//!
//! Two shapes, chosen by whether a `pty_request` preceded this one:
//!
//! - **No pty**: `tokio::process::Command`, fully async, stdin/stdout/stderr
//!   as ordinary pipes — this is the path every call from Faber takes today,
//!   since `SshTarget::bind_session` never advertises `Capability::Pty` for
//!   this transport (same manifest code the plain SSH path already uses).
//! - **Pty**: `portable-pty`. Its `Read`/`Write` handles are blocking, so
//!   they run on dedicated threads that bridge into the async world over
//!   channels — there is no async pty API to reach for instead (X40).
//!   Nothing on Faber's side asks for this yet; it exists so the daemon
//!   answers a real `ssh` client's `-t` the way a real sshd would, rather
//!   than being the one transport whose protocol surface silently narrows.
//!
//! Either way, the command is a *string* run through `sh -c`, once — the
//! same contract `quote()` in `ssh/mod.rs` exists to uphold on Faber's side,
//! mirrored here rather than re-derived: SSH carries one command string, and
//! the shell that parses it runs exactly once.

use std::process::Stdio;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use russh::ChannelId;
use russh::server::Handle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};

/// What `Handler::data` forwards client input to, for a channel currently
/// running a command.
pub enum Stdin {
    Piped(ChildStdin),
    Pty(tokio::sync::mpsc::UnboundedSender<Vec<u8>>),
}

impl Stdin {
    pub async fn write(&mut self, data: &[u8]) {
        match self {
            Stdin::Piped(stdin) => {
                let _ = stdin.write_all(data).await;
            }
            Stdin::Pty(tx) => {
                let _ = tx.send(data.to_vec());
            }
        }
    }
}

/// SSH extended data type 1 is stderr; there is no other in practice
/// (`ssh/mod.rs` reads the same constant back on Faber's side).
const STDERR: u32 = 1;

pub fn spawn_piped(
    channel_id: ChannelId,
    handle: Handle,
    command: String,
) -> Result<Stdin, std::io::Error> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");

    let out_handle = handle.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 32 * 1024];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_handle
                        .data(channel_id, buf[..n].to_vec())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    let err_handle = handle.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 32 * 1024];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if err_handle
                        .extended_data(channel_id, STDERR, buf[..n].to_vec())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    tokio::spawn(async move { finish(channel_id, handle, wait_piped(child).await).await });

    Ok(Stdin::Piped(stdin))
}

async fn wait_piped(mut child: Child) -> u32 {
    match child.wait().await {
        Ok(status) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(code) = status.code() {
                    return code as u32;
                }
                // Killed by a signal rather than exiting: 128+signal is the
                // shell convention, and the closest thing to a real answer
                // here since the protocol field is a bare number.
                128 + status.signal().unwrap_or(0) as u32
            }
            #[cfg(not(unix))]
            {
                status.code().unwrap_or(1) as u32
            }
        }
        Err(_) => 1,
    }
}

/// `size` is `None` only for a shell request with no prior `pty_request` —
/// callers only reach here once one has arrived, so this always allocates.
pub fn spawn_pty(
    channel_id: ChannelId,
    handle: Handle,
    command: Option<String>,
    size: PtySize,
) -> anyhow::Result<Stdin> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size)?;

    let mut cmd = match &command {
        Some(command) => {
            let mut cmd = CommandBuilder::new("sh");
            cmd.arg("-c");
            cmd.arg(command);
            cmd
        }
        None => CommandBuilder::new_default_prog(),
    };
    cmd.env("TERM", "xterm-256color");

    let child = pair.slave.spawn_command(cmd)?;
    // The slave fd belongs to the child now; holding it open past this point
    // only delays the EOF the master sees once the child's own copy closes.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // `try_clone_reader`/`take_writer` are blocking `Read`/`Write` — there is
    // no async pty API underneath them — so each direction gets its own
    // thread and crosses back into the async world over an unbounded
    // channel. `UnboundedSender::send` is a plain (non-async) method, which
    // is what makes it usable from a thread the tokio runtime doesn't know
    // about, unlike the bounded `Sender` this crate uses everywhere else.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 32 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let out_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(chunk) = out_rx.recv().await {
            if out_handle.data(channel_id, chunk).await.is_err() {
                break;
            }
        }
    });

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        while let Some(chunk) = rx.blocking_recv() {
            if writer.write_all(&chunk).is_err() {
                break;
            }
        }
    });

    // The master has to outlive the reader/writer threads above, or their
    // next syscall sees a closed fd instead of the child's actual output —
    // parking it in the wait task keeps it alive for exactly as long as
    // they need it and no longer.
    let master = pair.master;
    tokio::spawn(async move {
        let code = tokio::task::spawn_blocking(move || {
            let mut child = child;
            let code = child.wait().map(|status| status.exit_code()).unwrap_or(1);
            drop(master);
            code
        })
        .await
        .unwrap_or(1);
        finish(channel_id, handle, code).await;
    });

    Ok(Stdin::Pty(tx))
}

async fn finish(channel_id: ChannelId, handle: Handle, code: u32) {
    let _ = handle.exit_status_request(channel_id, code).await;
    let _ = handle.eof(channel_id).await;
    let _ = handle.close(channel_id).await;
}
