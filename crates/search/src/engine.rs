//! The capability itself: something that turns a [`Query`] into [`Results`].

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{Query, Results};

/// A search backend.
///
/// An engine holds a connection and whatever endpoint configuration it was
/// constructed with, and nothing else: no credential store, no cache, no
/// caller-visible retry knob. Implementations are handed to a harness inside
/// the capability object a workflow builds, which is why this is object-safe
/// (`Arc<dyn SearchEngine>`) and why every method takes `&self`.
#[async_trait]
pub trait SearchEngine: Send + Sync {
    /// Runs one query.
    ///
    /// Takes `&Query` rather than `Query` so a pool can hand the same query to
    /// a second instance after the first refuses, without cloning at every
    /// layer.
    async fn search(&self, query: &Query) -> Result<Results>;

    /// A name for logs and traces. Not an instance URL — a pool has many.
    fn provider(&self) -> &str;
}
