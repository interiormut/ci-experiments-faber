//! End-to-end: a stop raised from outside a live run, in a real isolate.
//!
//! The contract being pinned is `abstract.md`'s — a stop is not an unwind. It
//! reaches the harness as an ordinary failure of whatever outbound call was in
//! flight, carrying `types.d.ts`'s `cancelled` kind, and the harness decides
//! what to do about it. Every test here therefore asserts on what the harness
//! *saw*, not on the run merely having ended.

mod support;

use std::sync::Arc;
use std::time::Duration;

use harness::{HarnessRun, Interrupter, Seed};
use llm::{
    BlockStart, Delta, Event, EventStream, ModelClient, RenderedRequest,
    RenderedSpan, Request, ToolDef, UsageDelta,
};
use serde_json::{Value, json};
use support::{grant, input};

/// A model that starts answering and then never says another word — the
/// shape a stop actually has to interrupt. A script that simply ends would
/// let the run finish on its own and prove nothing.
struct Stalling;

impl ModelClient for Stalling {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix: RenderedSpan {
                provider: "scripted".into(),
                model: request.model.clone(),
                regions: Default::default(),
            },
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let opening = vec![
            Event::MessageStart {
                id: "msg_1".into(),
                model: "test-model".into(),
                usage: UsageDelta::default(),
            },
            Event::BlockStart {
                index: 0,
                block: BlockStart::Text,
            },
            Event::BlockDelta {
                index: 0,
                delta: Delta::Text {
                    content: "thinking about it".into(),
                },
            },
        ];
        Box::pin(async_stream::stream! {
            for event in opening {
                yield Ok(event);
            }
            std::future::pending::<()>().await;
        })
    }
}

/// Drains a run's transcript, raising the stop as soon as the run has
/// visibly started.
///
/// Tied to the first transcript event rather than a sleep: the point is that
/// the stop lands mid-answer, and a timer would sometimes land before the
/// isolate booted and test the pre-start path by accident.
fn drain_interrupting_at_first_event(
    run: HarnessRun,
    interrupter: Interrupter,
    timeout: Duration,
) -> (Vec<Value>, Result<harness::RunOutcome, harness::RunError>) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut run = run;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let events = runtime.block_on(async {
            let mut events: Vec<Value> = Vec::new();
            while let Some(event) = run.transcript.recv().await {
                if events.is_empty() {
                    interrupter.raise();
                }
                events.push(event);
            }
            events
        });
        let _ = tx.send((events, run.join()));
    });

    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        panic!("the run did not stop within {timeout:?} — a stop that hangs is worse than none")
    })
}

fn caught(events: &[Value]) -> Option<&Value> {
    events.iter().find(|event| event["type"] == "caught")
}

const CATCHING: &str = r#"
export default {
  execute: async function* (ctx, input) {
    try {
      const call = ctx.llm.stream({ messages: [...ctx.history.read(), ...input] });
      for await (const event of call) {
        yield event;
      }
    } catch (error) {
      yield { type: "caught", kind: error.kind };
    }
  }
};
"#;

#[test]
fn a_stop_mid_answer_reaches_the_harness_as_a_cancelled_failure() {
    let (interrupter, interrupt) = harness::interrupt();
    let mut grant = grant(Arc::new(Stalling));
    grant.interrupt = Some(interrupt);

    let run = HarnessRun::start(CATCHING.to_string(), input("hi"), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter, Duration::from_secs(10));

    // Ended by its own error handling, not by the isolate being killed: the
    // signal is the mechanism, and `join` returning `Ok` is what says the
    // backstop was never needed.
    let outcome = outcome.expect("a harness that handles the stop must still finish cleanly");

    let caught = caught(&events).expect("the harness must see the stop as a failure it can catch");
    assert_eq!(caught["kind"], "cancelled");

    // What did stream before the stop is still the user's conversation, and
    // the frame stopped failed because the call never produced a completion.
    assert!(
        events.iter().any(|event| event["type"] == "block_delta"),
        "text that arrived before the stop must survive it"
    );
    use harness::frame::{CoreEvent, Outcome};
    assert!(
        outcome.frames.iter().any(|frame| matches!(
            frame,
            CoreEvent::FrameStop {
                outcome: Outcome::Failed { error },
                ..
            } if error.kind == "cancelled"
        )),
        "the frame log must record why the call ended"
    );
}

const RETRYING: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const messages = [...ctx.history.read(), ...input];
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        for await (const event of ctx.llm.stream({ messages })) {
          yield event;
        }
      } catch (error) {
        yield { type: "caught", attempt, kind: error.kind };
      }
    }
  }
};
"#;

#[test]
fn a_stopped_run_cannot_open_another_call() {
    let (interrupter, interrupt) = harness::interrupt();
    let mut grant = grant(Arc::new(Stalling));
    grant.interrupt = Some(interrupt);

    let run = HarnessRun::start(RETRYING.to_string(), input("hi"), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter, Duration::from_secs(10));
    let outcome = outcome.expect("the retrying harness ends on its own once both attempts fail");

    let caught: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "caught")
        .collect();
    assert_eq!(caught.len(), 2, "both attempts must fail");
    assert_eq!(caught[1]["kind"], "cancelled");

    // Catching the stop is allowed; answering it by dialling the provider
    // again is not. The second attempt was refused at `llm.stream(...)`,
    // before a frame was ever opened for it.
    use harness::frame::CoreEvent;
    let opened = outcome
        .frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStart { .. }))
        .count();
    assert_eq!(opened, 1, "no second call may reach the provider");
}

const TOOL_USING: &str = r#"
export default {
  execute: async function* (ctx) {
    try {
      yield { type: "invoking" };
      await ctx.tools.invoke("wait", {});
      yield { type: "returned" };
    } catch (error) {
      yield { type: "caught", kind: error.kind };
    }
  }
};
"#;

#[test]
fn a_stop_during_a_tool_call_abandons_it() {
    let (interrupter, interrupt) = harness::interrupt();
    let mut grant = grant(Arc::new(Stalling));
    grant.interrupt = Some(interrupt);
    grant.tools = vec![ToolDef {
        name: "wait".into(),
        description: "never answers".into(),
        input_schema: json!({ "type": "object" }),
    }];
    // A tool that never returns: without a stop, this run does not end.
    grant.tool_invoker = Some(Arc::new(|_name, _input| {
        Box::pin(std::future::pending())
    }));

    let run = HarnessRun::start(TOOL_USING.to_string(), Vec::new(), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter, Duration::from_secs(10));
    outcome.expect("abandoning the tool call must leave the run able to finish");

    let caught = caught(&events).expect("the stop must surface at the awaited invocation");
    assert_eq!(caught["kind"], "cancelled");
    assert!(
        !events.iter().any(|event| event["type"] == "returned"),
        "an abandoned tool call must not appear to have returned"
    );
}

#[test]
fn a_run_stopped_before_it_starts_never_reaches_the_provider() {
    let (interrupter, interrupt) = harness::interrupt();
    interrupter.raise();

    let client = Arc::new(support::Scripted::new(support::text_reply("hello back")));
    let mut grant = grant(client.clone());
    grant.interrupt = Some(interrupt);

    // The run itself still boots — `crates/api` short-circuits earlier, but
    // the isolate has to behave if it doesn't, and the refusal lands at
    // `llm.stream(...)` where nothing has been sent yet.
    let run = HarnessRun::start(CATCHING.to_string(), input("hi"), grant, Seed::default());
    let (events, outcome) = drain_interrupting_at_first_event(
        run,
        interrupter.clone(),
        Duration::from_secs(10),
    );
    outcome.expect("a refusal at open is an ordinary failure, not a broken run");

    assert_eq!(
        caught(&events).map(|event| event["kind"].clone()),
        Some(json!("cancelled"))
    );
    assert!(
        client.requests_seen().is_empty(),
        "a run stopped before it opened a call must render nothing"
    );
}

#[test]
fn killing_the_isolate_stops_a_harness_that_ignores_the_signal() {
    // The backstop `history-abstract.md` H9.3 describes: this harness never
    // touches an op again after its first, so no signal can reach it, and
    // only terminating the isolate ends the run.
    const SPINNING: &str = r#"
export default {
  execute: async function* () {
    yield { type: "spinning" };
    for (;;) {}
  }
};
"#;

    let run = HarnessRun::start(
        SPINNING.to_string(),
        Vec::new(),
        grant(Arc::new(Stalling)),
        Seed::default(),
    );
    let terminator = run.terminator();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut run = run;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async { while run.transcript.recv().await.is_some() {} });
        let _ = tx.send(run.join().is_err());
    });

    std::thread::sleep(Duration::from_millis(200));
    terminator.terminate();

    let ended_in_error = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("terminating the isolate must end the run");
    assert!(
        ended_in_error,
        "a killed run yields no outcome — `crates/api` recovers its text instead"
    );
}
