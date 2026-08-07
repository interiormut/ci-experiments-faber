//! The capability itself: something that turns a [`Request`] into a stream of
//! [`Event`]s.

use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;
use url::Url;

use crate::error::{Error, Result};
use crate::event::{Accumulator, Completion, Event};
use crate::span::RenderedRequest;
use crate::types::Request;

/// A stream of events from one model call.
pub type EventStream<'a> = Pin<Box<dyn Stream<Item = Result<Event>> + Send + 'a>>;

/// Appends an API path to a base URL, keeping any path the base already has.
///
/// `Url::join` with an absolute path would discard it, silently misrouting the
/// gateway and proxy URLs that are the whole reason `base_url` is
/// configurable: `https://gw.example.com/openai` has to stay under `/openai`.
pub(crate) fn endpoint<'a>(
    mut base: Url,
    segments: impl IntoIterator<Item = &'a str>,
) -> Result<Url> {
    {
        let mut path = base
            .path_segments_mut()
            .map_err(|()| Error::Decode("base URL must be http or https".into()))?;
        // A base of `https://host/openai` has a trailing empty segment only
        // when it was written with a slash; popping it keeps both spellings
        // from producing `/openai//v1`.
        path.pop_if_empty().extend(segments);
    }
    Ok(base)
}

/// A model endpoint.
///
/// A client holds a connection and a credential and nothing else: no retry
/// policy, no context management, no tool dispatch, no notion of a turn.
/// Implementations are handed to a harness through the capability object a
/// workflow constructs, which is why this is object-safe and why every
/// method takes `&self`.
pub trait ModelClient: Send + Sync {
    /// Turns a [`Request`] into provider bytes, without sending them.
    ///
    /// Split from [`send`](ModelClient::send) so the caller can capture
    /// [`RenderedRequest::prefix`] before or independently of dispatch — a
    /// harness inspecting what would be sent, or the frame log recording
    /// ground truth at the capability boundary (J1), needs the bytes whether
    /// or not the call ever runs.
    fn render(&self, request: &Request) -> Result<RenderedRequest>;

    /// Sends bytes [`render`](ModelClient::render) already produced.
    fn send(&self, rendered: RenderedRequest) -> EventStream<'_>;

    /// Starts a call. The request is not sent until the stream is polled.
    ///
    /// The one-call convenience path: render then send. A caller that wants
    /// the rendered prefix span calls the two methods directly instead.
    fn stream(&self, request: Request) -> EventStream<'_> {
        match self.render(&request) {
            Ok(rendered) => self.send(rendered),
            Err(error) => Box::pin(futures_util::stream::once(async move { Err(error) })),
        }
    }

    /// A name for logs and traces. Not a model id.
    fn provider(&self) -> &str;
}

/// Runs a request to completion, discarding intermediate events.
///
/// The convenience path for callers that don't render deltas. Streaming is
/// still used underneath so long generations don't trip request timeouts.
pub async fn complete(client: &dyn ModelClient, request: Request) -> Result<Completion> {
    let mut stream = client.stream(request);
    let mut accumulator = Accumulator::new();
    while let Some(event) = stream.next().await {
        accumulator.push(&event?);
    }
    accumulator.finish()
}

/// Drives a stream while handing each event to `observer`, then returns the
/// completed message.
///
/// The observer sees events as they land; a failure ends the call.
pub async fn complete_observed<F>(
    client: &dyn ModelClient,
    request: Request,
    mut observer: F,
) -> Result<Completion>
where
    F: FnMut(&Event),
{
    let mut stream = client.stream(request);
    let mut accumulator = Accumulator::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        observer(&event);
        accumulator.push(&event);
    }
    accumulator.finish()
}
