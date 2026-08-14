//! Model connectivity for Faber's core.
//!
//! One trait — [`ModelClient`] — turns a provider-neutral [`Request`] into a
//! stream of [`Event`]s, and one provider implements it. Everything above the
//! wire is deliberately absent:
//!
//! - **No retry, no backoff.** Failures are values ([`Error`]); when to try
//!   again is the harness's decision.
//! - **No context management.** Nothing here counts tokens or drops turns.
//!   [`Thread`] holds exactly what a caller put in it.
//! - **No tool loop.** Tool calls are surfaced as content; dispatching them is
//!   the harness's, with whatever the workflow granted it.
//! - **No role resolution.** [`Request::model`] is a model id; roles resolve
//!   above this crate.
//! - **No ambient authority.** A client reads no environment and holds no
//!   globals; a credential arrives through its constructor or not at all.
//!
//! The zero-configuration path is two lines:
//!
//! ```no_run
//! # use llm::{Message, Request, Thread, anthropic};
//! # use secrecy::SecretString;
//! # async fn run(key: SecretString) -> Result<(), llm::Error> {
//! let client = anthropic::Anthropic::new(anthropic::Config::new(key))?;
//! let answer = llm::complete(
//!     &client,
//!     Request::new(anthropic::DEFAULT_MODEL, vec![Message::user("hello")]),
//! )
//! .await?;
//! println!("{}", answer.text());
//!
//! // Or against a conversation, which reads history and appends to it.
//! let mut thread = Thread::new();
//! thread.push_user("hello");
//! let request = thread.request(anthropic::DEFAULT_MODEL)?;
//! thread.call(&client, request).await?;
//! # Ok(())
//! # }
//! ```

pub mod anthropic;
mod client;
mod error;
mod event;
pub mod openai;
mod span;
mod sse;
mod thread;
mod types;

pub use client::{EventStream, ModelClient, complete, complete_observed};
pub use error::{Error, Result};
pub use event::{Accumulator, BlockStart, Completion, Delta, Event};
pub use span::{RenderedRequest, RenderedSpan};
pub use thread::Thread;
pub use types::{
    ContentBlock, DEFAULT_MAX_TOKENS, Effort, Message, ReasoningHistory, Request, Role, Sampling,
    StopDetails, StopReason, Thinking, ThinkingDisplay, ToolChoice, ToolDef, Turn, Usage,
    UsageDelta, leading_system_run,
};
