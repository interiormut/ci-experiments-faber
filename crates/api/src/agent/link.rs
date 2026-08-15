//! Wraps a WebSocket as `AsyncRead + AsyncWrite`, the seam
//! `SshSession::from_stream` (X39) needs and nothing more — `environment`
//! carries no WebSocket dependency, so the adapter lives here instead.
//!
//! SSH protocol bytes ride inside `Binary` frames. This also owns the one
//! liveness mechanism this connection gets: R16's second consequence is that
//! nothing detects a wedged-but-connected daemon unless something here
//! pings for it, since [`SshSession`] is built with `inactivity_timeout:
//! None` and R14 forbids probing the way a dialed host would be. Once a
//! stream is handed to `SshSession::from_stream`, russh owns it exclusively
//! from a single background task, so a ping/pong deadline has to live
//! inside the adapter itself — there is no other point still holding a
//! handle to the raw connection to send one from.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Sleep;

/// How often an idle connection gets pinged.
const PING_INTERVAL: Duration = Duration::from_secs(20);

/// How long a ping may go unanswered before the link is declared dead.
/// Three missed intervals: one late pong is jitter, three is a half-open
/// proxy silently dropping one direction — the exact failure this transport
/// exists to survive, and the exact one it needs to notice.
const PING_TIMEOUT: Duration = Duration::from_secs(60);

pub struct WsStream {
    ws: WebSocket,
    /// Bytes from a `Binary` frame not yet consumed by the caller.
    pending: Bytes,
    last_pong: Instant,
    /// Fires every [`PING_INTERVAL`] and is reset, not left to expire once.
    /// Boxed and pinned rather than borrowed off the stack: a `tokio::time`
    /// timer, once polled, has to stay at the address it registered its
    /// waker against, and `WsStream` itself has to stay [`Unpin`] — the
    /// stream `from_stream` in `environment::ssh` requires — so the pinning
    /// happens here instead of on the whole struct.
    ///
    /// Driving this from `poll_read` is what makes the ping fire on an idle
    /// connection at all. Without a timer registering its own wakeup, a
    /// `WsStream` with nothing arriving simply never gets `poll_read`ed
    /// again — there is no incoming data to wake it, and a wedged link
    /// would look identical to a quiet one.
    ping_timer: Pin<Box<Sleep>>,
    /// A ping was accepted by the sink but `poll_flush` returned `Pending`.
    /// Retried at the top of the next `poll_read` rather than dropped —
    /// otherwise a ping that didn't flush on its first attempt never goes
    /// out, and the deadline it was meant to keep alive fires on a link
    /// that was never actually unresponsive.
    ping_unflushed: bool,
}

impl WsStream {
    pub fn new(ws: WebSocket) -> Self {
        Self {
            ws,
            pending: Bytes::new(),
            last_pong: Instant::now(),
            ping_timer: Box::pin(tokio::time::sleep(PING_INTERVAL)),
            ping_unflushed: false,
        }
    }

    /// Retries a flush left over from a prior ping. Read as: "is the sink
    /// clear to send on", not "is there new data" — a `Pending` here just
    /// means try again next poll, the same as any other write backpressure.
    fn retry_unflushed_ping(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.ping_unflushed {
            return Poll::Ready(Ok(()));
        }
        match Pin::new(&mut self.ws).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                self.ping_unflushed = false;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(io::Error::other(error))),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Sends a ping when the timer fires, best-effort: a full send buffer or
    /// a `Pending` `poll_ready` just means this interval's ping is skipped,
    /// not a reason to fail the read — the next interval tries again, and
    /// only a sustained silence trips [`PING_TIMEOUT`].
    fn maybe_ping(&mut self, cx: &mut Context<'_>) {
        if self.ping_timer.as_mut().poll(cx).is_pending() {
            return;
        }
        self.ping_timer
            .as_mut()
            .reset(tokio::time::Instant::now() + PING_INTERVAL);

        if Pin::new(&mut self.ws).poll_ready(cx).is_ready()
            && Pin::new(&mut self.ws)
                .start_send(Message::Ping(Bytes::new()))
                .is_ok()
        {
            self.ping_unflushed = true;
            let _ = self.retry_unflushed_ping(cx);
        }
    }
}

impl AsyncRead for WsStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if Instant::now() >= self.last_pong + PING_TIMEOUT {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "agent link missed its ping deadline",
            )));
        }
        if let Poll::Ready(Err(error)) = self.retry_unflushed_ping(cx) {
            return Poll::Ready(Err(error));
        }
        self.maybe_ping(cx);

        loop {
            if !self.pending.is_empty() {
                let take = self.pending.len().min(buf.remaining());
                buf.put_slice(&self.pending[..take]);
                self.pending = self.pending.slice(take..);
                return Poll::Ready(Ok(()));
            }

            match Pin::new(&mut self.ws).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                    if data.is_empty() {
                        continue;
                    }
                    self.pending = data;
                }
                // `Ping` is answered by axum/tungstenite before it ever
                // reaches here; `Text` has no place in this protocol and is
                // dropped rather than treated as EOF or an error, since a
                // stray one is not what makes the transport unusable.
                Poll::Ready(Some(Ok(Message::Pong(_)))) => {
                    self.last_pong = Instant::now();
                }
                Poll::Ready(Some(Ok(Message::Ping(_) | Message::Text(_)))) => {}
                Poll::Ready(Some(Ok(Message::Close(_)))) | Poll::Ready(None) => {
                    return Poll::Ready(Ok(())); // EOF: an unfilled buf is the signal.
                }
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Err(io::Error::other(error)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for WsStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.ws).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
            Poll::Pending => return Poll::Pending,
        }
        Pin::new(&mut self.ws)
            .start_send(Message::Binary(Bytes::copy_from_slice(buf)))
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.ws).poll_flush(cx).map_err(io::Error::other);
        if result.is_ready() {
            // Whatever was queued — including a ping sent from `poll_read`
            // that didn't flush on its first attempt — just did.
            self.ping_unflushed = false;
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_close(cx).map_err(io::Error::other)
    }
}
