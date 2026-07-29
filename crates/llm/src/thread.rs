//! A conversation the model call reads from and appends to.
//!
//! Purity would be tidier here and would break the one-line harness, which is
//! not a trade available to us: [`Thread::call`] reads history, appends the
//! user turn's answer, and hands the completion back. What it will not do is
//! decide what happens next — no retry, no tool dispatch, no trimming.

use crate::client::{ModelClient, complete, complete_observed};
use crate::error::{Error, Result};
use crate::event::{Completion, Event};
use crate::types::{ContentBlock, Message, Request, Role};

/// A forkable conversation.
#[derive(Clone, Debug, Default)]
pub struct Thread {
    messages: Vec<Message>,
}

impl Thread {
    pub fn new() -> Self {
        Self::default()
    }

    /// A thread with a system prompt, as its leading turn.
    pub fn with_system(system: impl Into<String>) -> Self {
        Self {
            messages: vec![Message::system(system)],
        }
    }

    /// A copy that shares no state with the original. The point of forking is
    /// speculative work: run a branch, keep it or drop it.
    pub fn fork(&self) -> Self {
        self.clone()
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn push(&mut self, message: Message) -> &mut Self {
        self.messages.push(message);
        self
    }

    pub fn push_user(&mut self, text: impl Into<String>) -> &mut Self {
        self.push(Message::user(text))
    }

    /// Appends an operator instruction as its own turn. On a provider that
    /// supports mid-conversation system messages this lands after the
    /// history without disturbing it; elsewhere the provider's wire layer
    /// decides how to represent it.
    pub fn push_system(&mut self, text: impl Into<String>) -> &mut Self {
        self.push(Message::system(text))
    }

    /// Appends tool results as a single user turn, which is how every current
    /// provider expects them: one turn carrying every result, not one turn
    /// each.
    pub fn push_tool_results(&mut self, results: Vec<ContentBlock>) -> &mut Self {
        self.push(Message {
            role: Role::User,
            content: results,
        })
    }

    /// Truncates the thread to its first `len` turns.
    ///
    /// This is a caller-driven edit, not context management — the host never
    /// drops turns on its own.
    pub fn truncate(&mut self, len: usize) -> &mut Self {
        self.messages.truncate(len);
        self
    }

    /// Builds a request against the current history without sending it.
    ///
    /// Useful for a harness that wants to set `max_tokens`, tools, or effort
    /// before calling, and for inspecting exactly what would be sent.
    pub fn request(&self, model: impl Into<String>) -> Result<Request> {
        self.guard_trailing_assistant()?;
        Ok(Request::new(model, self.messages.clone()))
    }

    /// Calls the model with the given request and appends its answer.
    ///
    /// `request.messages` is **replaced** by the thread's, unconditionally —
    /// the thread is the single source of what gets sent, so editing a
    /// request's messages before calling has no effect. Everything else on
    /// the request is honoured as given. A harness that wants a one-off turn
    /// the thread doesn't keep should call [`complete`] directly with a
    /// request it built itself, or fork.
    pub async fn call(
        &mut self,
        client: &dyn ModelClient,
        mut request: Request,
    ) -> Result<Completion> {
        self.guard_trailing_assistant()?;
        request.messages = self.messages.clone();

        let completion = complete(client, request).await?;
        self.messages.push(completion.message.clone());
        Ok(completion)
    }

    /// [`Thread::call`], with each event handed to `observer` as it lands.
    pub async fn call_observed<F>(
        &mut self,
        client: &dyn ModelClient,
        mut request: Request,
        observer: F,
    ) -> Result<Completion>
    where
        F: FnMut(&Event),
    {
        self.guard_trailing_assistant()?;
        request.messages = self.messages.clone();

        let completion = complete_observed(client, request, observer).await?;
        self.messages.push(completion.message.clone());
        Ok(completion)
    }

    /// A trailing assistant turn is a prefill, which current models reject.
    /// Better a typed error at this boundary than a 400 from the provider.
    fn guard_trailing_assistant(&self) -> Result<()> {
        match self.messages.last() {
            Some(Message {
                role: Role::Assistant,
                ..
            }) => Err(Error::ExpectedUserTurn),
            _ => Ok(()),
        }
    }
}
