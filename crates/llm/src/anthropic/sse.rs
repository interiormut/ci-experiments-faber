//! Server-sent event framing.
//!
//! Only what the Messages API actually uses: `data:` lines terminated by a
//! blank line. The `event:` line is redundant — every payload repeats its type
//! in the JSON — so it is ignored.

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{Error, Result};

/// Turns a byte stream into one JSON value per SSE frame.
pub(super) fn frames<S>(bytes: S) -> impl Stream<Item = Result<Value>>
where
    S: Stream<Item = reqwest::Result<Bytes>>,
{
    async_stream::try_stream! {
        let mut bytes = Box::pin(bytes);
        let mut buffer = String::new();

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(split) = find_frame_end(&buffer) {
                let frame = buffer[..split.start].to_owned();
                buffer.drain(..split.end);
                if let Some(value) = decode(&frame)? {
                    yield value;
                }
            }
        }

        // A stream cut short mid-frame is a truncated response, not an empty
        // one: report it rather than returning a partial message.
        if !buffer.trim().is_empty() {
            Err(Error::Decode(
                "stream ended mid-frame; the response is incomplete".into(),
            ))?;
        }
    }
}

struct FrameEnd {
    start: usize,
    end: usize,
}

/// Finds the blank line ending the first complete frame, tolerating CRLF.
fn find_frame_end(buffer: &str) -> Option<FrameEnd> {
    let lf = buffer.find("\n\n").map(|at| FrameEnd {
        start: at,
        end: at + 2,
    });
    let crlf = buffer.find("\r\n\r\n").map(|at| FrameEnd {
        start: at,
        end: at + 4,
    });
    match (lf, crlf) {
        (Some(lf), Some(crlf)) if crlf.start < lf.start => Some(crlf),
        (Some(lf), _) => Some(lf),
        (None, crlf) => crlf,
    }
}

/// Concatenates a frame's `data:` lines and parses the result.
///
/// Returns `None` for a frame carrying no data (a bare comment or the
/// terminal `[DONE]` sentinel some providers send).
fn decode(frame: &str) -> Result<Option<Value>> {
    let mut data = String::new();
    for line in frame.lines() {
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
    }

    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }

    serde_json::from_str(data)
        .map(Some)
        .map_err(|source| Error::Decode(format!("malformed stream frame: {source}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    async fn collect(chunks: Vec<&'static str>) -> Vec<Result<Value>> {
        let bytes = stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok(Bytes::from_static(chunk.as_bytes()))),
        );
        frames(bytes).collect().await
    }

    #[tokio::test]
    async fn frames_split_across_chunks_are_rejoined() {
        let events = collect(vec![
            "event: message_start\ndata: {\"type\":\"mes",
            "sage_start\"}\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\n",
        ])
        .await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap()["type"], "message_start");
        assert_eq!(events[1].as_ref().unwrap()["type"], "ping");
    }

    #[tokio::test]
    async fn crlf_framing_works() {
        let events = collect(vec!["data: {\"type\":\"message_stop\"}\r\n\r\n"]).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap()["type"], "message_stop");
    }

    #[tokio::test]
    async fn a_truncated_stream_is_an_error() {
        let events = collect(vec!["data: {\"type\":\"message_st"]).await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Err(Error::Decode(_))));
    }

    #[tokio::test]
    async fn comment_only_frames_are_skipped() {
        let events = collect(vec![": keepalive\n\ndata: {\"type\":\"message_stop\"}\n\n"]).await;
        assert_eq!(events.len(), 1);
    }
}
