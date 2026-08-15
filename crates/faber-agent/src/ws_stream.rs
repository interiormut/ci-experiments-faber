//! The daemon's half of the WebSocket-as-`AsyncRead+AsyncWrite` seam — see
//! `crates/api/src/agent/link.rs` for the broker's half. Deliberately not
//! shared code: different crate, different WebSocket type
//! (`tokio_tungstenite::WebSocketStream` here, `axum::extract::ws::WebSocket`
//! there), and this side answers pings rather than sending them — the
//! broker owns the one liveness clock for this connection (X38), and two
//! clocks pinging each other independently would just be two things that
//! can disagree about whether the link is alive.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

pub struct WsStream<S> {
    ws: WebSocketStream<S>,
    pending: Bytes,
}

impl<S> WsStream<S> {
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self {
            ws,
            pending: Bytes::new(),
        }
    }
}

impl<S> AsyncRead for WsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
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
                // Pings are answered by tungstenite before this ever sees
                // them; a stray Text or Pong (the broker's own liveness
                // check, not ours to act on) is dropped rather than treated
                // as data or as EOF.
                Poll::Ready(Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_)))) => {
                }
                Poll::Ready(Some(Ok(Message::Close(_) | Message::Frame(_)))) | Poll::Ready(None) => {
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

impl<S> AsyncWrite for WsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
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
        // `start_send` only queues the frame in tungstenite's own buffer;
        // nothing reaches the socket until something flushes it, and an
        // `AsyncWrite` caller is not obliged to call `poll_flush` on its own
        // after every `poll_write`. Best-effort — a `Pending` here just means
        // the next write or an explicit flush tries again.
        let _ = Pin::new(&mut self.ws).poll_flush(cx);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_flush(cx).map_err(io::Error::other)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_close(cx).map_err(io::Error::other)
    }
}
