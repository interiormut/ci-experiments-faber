//! The capability itself: something that turns a [`Request`] into a stream of
//! [`Event`]s.

use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;

use crate::error::Result;
use crate::event::{Accumulator, Completion, Event};
use crate::types::Request;

/// A stream of events from one model call.
pub type EventStream<'a> = Pin<Box<dyn Stream<Item = Result<Event>> + Send + 'a>>;

/// A model endpoint.
///
/// Deliberately one method. A client holds a connection and a credential and
/// nothing else: no retry policy, no context management, no tool dispatch, no
/// notion of a turn. Implementations are handed to a harness through the
/// capability object a workflow constructs, which is why this is object-safe
/// and why it takes `&self`.
pub trait ModelClient: Send + Sync {
    /// Starts a call. The request is not sent until the stream is polled.
    fn stream(&self, request: Request) -> EventStream<'_>;

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
