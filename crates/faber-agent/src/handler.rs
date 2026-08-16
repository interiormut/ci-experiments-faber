//! This daemon's `russh::server::Handler` — the inverse of
//! `LocalSpawn`/`LocalFiles` (X40): it runs as the far end of a connection
//! instead of the near end, spawning local processes and touching the local
//! filesystem in response to whatever Faber's `SshSpawn`/`SftpFiles`/
//! `SshForwarded` ask of it.
//!
//! **Not an authentication boundary.** `auth_none` accepts unconditionally,
//! on R15's argument that the WebSocket hop this session arrived over
//! already established identity (the machine-auth extractor in
//! `crates/api/src/agent/auth.rs`, one layer below). That is only sound
//! because this daemon never listens on a socket anything else can reach —
//! it dials out, and every connection it serves is the one Faber gave it.

use std::collections::HashMap;

use portable_pty::PtySize;
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, Pty};

use crate::exec::{self, Stdin};

#[derive(Default)]
pub struct Handler {
    /// Channels opened but not yet claimed by an exec/shell/subsystem
    /// request — `channel_open_session` doesn't know yet which of those
    /// this will become.
    pending: HashMap<ChannelId, Channel<Msg>>,
    /// A `pty_request` that arrived before the exec/shell request it
    /// describes. SSH allows (and OpenSSH's own client sends) `pty-req`
    /// ahead of `exec`/`shell`, never after.
    pty_size: HashMap<ChannelId, PtySize>,
    /// Where `Handler::data` forwards a channel's input, once something is
    /// actually running on it.
    stdin: HashMap<ChannelId, Stdin>,
}

impl russh::server::Handler for Handler {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pending.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty_size.insert(
            channel,
            PtySize {
                rows: row_height as u16,
                cols: col_width as u16,
                pixel_width: pix_width as u16,
                pixel_height: pix_height as u16,
            },
        );
        session.channel_success(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Best-effort acknowledgement only: the live resize would need a
        // handle back to the pty's master kept past `exec.rs`'s wait task,
        // which nothing here holds. A running command sees its original
        // size for the rest of its life rather than SIGWINCH — a real gap,
        // not a design choice, and nothing Faber exercises today notices it
        // (agent mode carries no `Capability::Pty` — see `exec.rs`).
        let _ = (col_width, row_height, pix_width, pix_height);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Consumed and dropped, not held: once accepted, a channel is
        // driven entirely through `Handler::data` (input) and `Handle`
        // (output) — dropping the `Channel<Msg>` object itself does not
        // close it, the same way the echo-server example in `russh`
        // discards its `channel` right after registering the id.
        self.pending.remove(&channel);
        let command = String::from_utf8_lossy(data).into_owned();
        let handle = session.handle();

        let stdin = match self.pty_size.remove(&channel) {
            Some(size) => exec::spawn_pty(channel, handle, Some(command), size)?,
            None => exec::spawn_piped(channel, handle, command)?,
        };
        self.stdin.insert(channel, stdin);

        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pending.remove(&channel);
        let handle = session.handle();

        // A shell with no pty is not what an interactive client wants, but
        // it is still a coherent thing to serve — piped stdio, same as
        // `exec` without one.
        let stdin = match self.pty_size.remove(&channel) {
            Some(size) => exec::spawn_pty(channel, handle, None, size)?,
            None => exec::spawn_piped(channel, handle, default_shell())?,
        };
        self.stdin.insert(channel, stdin);

        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(stdin) = self.stdin.get_mut(&channel) {
            stdin.write(data).await;
        }
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let Some(stream_channel) = self.pending.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        tokio::spawn(async move {
            russh_sftp::server::run(stream_channel.into_stream(), crate::sftp::Sftp::default())
                .await;
        });
        Ok(())
    }

    /// `agent+docker` (X39): Faber's `SshForwarded` opens this to reach the
    /// container daemon's socket, exactly as it would over a dialed SSH
    /// connection — the far end doesn't know or care which kind of
    /// connection it's speaking over, only that this is `direct-streamlocal`
    /// naming a path on this machine.
    async fn channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(mut socket) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut socket).await;
                });
            }
            Err(error) => {
                tracing::warn!(%socket_path, %error, "could not reach the local socket");
                reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            }
        }
        Ok(())
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}
