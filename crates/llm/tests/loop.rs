//! The two cases §4 says must stay trivial: a harness that does nothing but
//! pass its input to the model, and a conversation that reads history and
//! appends to it.

use llm::{
    BlockStart, ContentBlock, Delta, Event, EventStream, Message, ModelClient, RenderedRequest,
    RenderedSpan, Request, Role, StopReason, Thread, Turn, UsageDelta, complete,
};

/// A model that replays a fixed script and records what it was asked.
struct Scripted {
    events: Vec<Event>,
    seen: std::sync::Mutex<Vec<Request>>,
}

impl Scripted {
    fn new(events: Vec<Event>) -> Self {
        Self {
            events,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn last_request(&self) -> Request {
        self.seen.lock().unwrap().last().cloned().unwrap()
    }
}

/// Pulls the plain [`Message`] out of a [`Turn`], for tests that only ever
/// deal in by-value turns and never resume a span.
fn message_at(request: &Request, index: usize) -> &Message {
    match &request.messages[index] {
        Turn::Value(message) => message,
        Turn::Span(_) => panic!("expected a plain message at index {index}, found a span"),
    }
}

impl ModelClient for Scripted {
    fn provider(&self) -> &str {
        "scripted"
    }

    // No wire format to render against: this fake records the request as
    // given and replays a fixed script regardless, so `render` just captures
    // it and `send` ignores the (empty) rendered body entirely.
    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        self.seen.lock().unwrap().push(request.clone());
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix: RenderedSpan {
                provider: "scripted".into(),
                model: request.model.clone(),
                reasoning: request
                    .reasoning_history
                    .unwrap_or(llm::ReasoningHistory::Full),
                regions: Default::default(),
            },
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let events = self.events.clone();
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }
}

fn text_reply(text: &str) -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_1".into(),
            model: "test-model".into(),
            usage: UsageDelta {
                input_tokens: Some(10),
                ..UsageDelta::default()
            },
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::Text,
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::Text {
                content: text.into(),
            },
        },
        Event::BlockStop { index: 0 },
        Event::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            stop_details: None,
            usage: UsageDelta {
                output_tokens: Some(4),
                ..UsageDelta::default()
            },
        },
        Event::MessageStop,
    ]
}

#[tokio::test]
async fn the_bare_call_is_one_line() {
    let client = Scripted::new(text_reply("hello back"));
    let answer = complete(
        &client,
        Request::new("test-model", vec![Message::user("hello")]),
    )
    .await
    .unwrap();

    assert_eq!(answer.text(), "hello back");
    assert_eq!(answer.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(answer.usage.input_tokens, 10);
    assert_eq!(answer.usage.output_tokens, 4);
}

#[tokio::test]
async fn a_thread_reads_history_and_appends() {
    let client = Scripted::new(text_reply("second"));
    let mut thread = Thread::with_system("be brief");
    thread.push_user("first");

    let request = thread.request("test-model").unwrap();
    thread.call(&client, request).await.unwrap();

    assert_eq!(thread.messages().len(), 3);
    assert_eq!(thread.messages()[0].role, Role::System);
    assert_eq!(thread.messages()[2].role, Role::Assistant);
    assert_eq!(thread.messages()[2].text(), "second");

    // The system prompt travels with the call as a leading turn.
    let last_request = client.last_request();
    assert_eq!(last_request.messages.len(), 2);
    assert_eq!(message_at(&last_request, 0).role, Role::System);
}

#[tokio::test]
async fn a_fork_does_not_disturb_its_parent() {
    let client = Scripted::new(text_reply("branch"));
    let mut thread = Thread::new();
    thread.push_user("first");

    let mut fork = thread.fork();
    let request = fork.request("test-model").unwrap();
    fork.call(&client, request).await.unwrap();

    assert_eq!(fork.messages().len(), 2);
    assert_eq!(thread.messages().len(), 1);
}

#[tokio::test]
async fn calling_twice_without_a_user_turn_is_a_typed_error() {
    let client = Scripted::new(text_reply("first answer"));
    let mut thread = Thread::new();
    thread.push_user("hello");
    let request = thread.request("test-model").unwrap();
    thread.call(&client, request.clone()).await.unwrap();

    // The provider would reject this as a prefill; catch it at the boundary.
    let error = thread.call(&client, request).await.unwrap_err();
    assert!(matches!(error, llm::Error::ExpectedUserTurn));

    // Tool results are a user turn, so the loop continues from there.
    thread.push_tool_results(vec![ContentBlock::tool_result("toolu_1", "42")]);
    assert!(thread.request("test-model").is_ok());
}

#[tokio::test]
async fn tool_calls_arrive_whole() {
    let client = Scripted::new(vec![
        Event::MessageStart {
            id: "msg_2".into(),
            model: "test-model".into(),
            usage: UsageDelta::default(),
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::ToolUse {
                id: "toolu_1".into(),
                name: "read_file".into(),
            },
        },
        // Streamed in fragments, as the wire delivers them.
        Event::BlockDelta {
            index: 0,
            delta: Delta::ToolInputJson {
                content: "{\"path\":".into(),
            },
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::ToolInputJson {
                content: "\"/tmp/x\"}".into(),
            },
        },
        Event::BlockStop { index: 0 },
        Event::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            stop_details: None,
            usage: UsageDelta::default(),
        },
        Event::MessageStop,
    ]);

    let answer = complete(
        &client,
        Request::new("test-model", vec![Message::user("read /tmp/x")]),
    )
    .await
    .unwrap();

    let calls: Vec<_> = answer.tool_uses().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "read_file");
    assert_eq!(calls[0].2["path"], "/tmp/x");
    // The stop reason is reported, not acted on — no tool was dispatched.
    assert_eq!(answer.stop_reason, Some(StopReason::ToolUse));
}

#[tokio::test]
async fn a_stream_that_says_nothing_is_a_failure_not_an_empty_answer() {
    let client = Scripted::new(Vec::new());
    let error = complete(
        &client,
        Request::new("test-model", vec![Message::user("hello")]),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, llm::Error::EmptyResponse));
}

#[tokio::test]
async fn a_reported_zero_is_recorded_as_zero() {
    // A refusal declined before any output really did produce nothing.
    let client = Scripted::new(vec![
        Event::MessageStart {
            id: "msg_3".into(),
            model: "test-model".into(),
            usage: UsageDelta {
                input_tokens: Some(9),
                output_tokens: Some(7),
                ..UsageDelta::default()
            },
        },
        Event::MessageDelta {
            stop_reason: Some(StopReason::Refusal),
            stop_details: Some(llm::StopDetails {
                category: Some("cyber".into()),
                explanation: None,
            }),
            usage: UsageDelta {
                output_tokens: Some(0),
                ..UsageDelta::default()
            },
        },
        Event::MessageStop,
    ]);

    let answer = complete(
        &client,
        Request::new("test-model", vec![Message::user("hello")]),
    )
    .await
    .unwrap();

    assert_eq!(answer.usage.input_tokens, 9, "unreported fields hold");
    assert_eq!(answer.usage.output_tokens, 0, "reported zero overwrites");
    assert_eq!(answer.stop_reason, Some(StopReason::Refusal));
    assert_eq!(
        answer.stop_details.unwrap().category.as_deref(),
        Some("cyber")
    );
}
