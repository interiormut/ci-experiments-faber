//! End-to-end: a real deno_core isolate running real harness JavaScript
//! against a scripted `llm::ModelClient`. No network, fully deterministic.

mod support;

use std::sync::Arc;
use std::time::Duration;

use harness::{HarnessRun, Seed};
use support::{Recording, Scripted, drain_transcript, grant, input, text_reply};

const IDENTITY: &str = r#"
export default {
  execute: (ctx, input) => ctx.llm.stream({ messages: [...ctx.history.read(), input] })
};
"#;

#[test]
fn identity_harness_end_to_end_matches_the_script() {
    let client = Arc::new(Scripted::new(text_reply("hello back")));
    let mut run = HarnessRun::start(
        IDENTITY.to_string(),
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );

    let events = drain_transcript(&mut run);
    let frames = run.join().expect("run must finish cleanly").frames;

    assert!(!events.is_empty(), "the identity harness must forward events");
    assert_eq!(events[0]["type"], "message_start");
    assert_eq!(events[0]["model"], "test-model");

    let text_delta = events
        .iter()
        .find(|event| event["type"] == "block_delta")
        .expect("a text delta must appear");
    assert_eq!(text_delta["delta"]["type"], "text");
    assert_eq!(text_delta["delta"]["text"], "hello back");

    assert_eq!(events.last().unwrap()["type"], "message_stop");

    // The frame log recorded one model frame: a start, one ModelEvent per
    // provider event, a usage record, and a stop.
    use harness::frame::CoreEvent;
    let starts = frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStart { .. }))
        .count();
    let stops = frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStop { outcome, .. } if matches!(outcome, harness::frame::Outcome::Ok)))
        .count();
    assert_eq!((starts, stops), (1, 1));
}

#[test]
fn a_recording_client_renders_real_provider_bytes_not_a_fixture() {
    // `Recording` (unlike `Scripted`) delegates rendering to the real
    // Anthropic wire module, so the bytes it captures are the provider's own
    // — this is the fixture the byte-identity tests in `history.rs` depend
    // on. Proven here, against the identity harness, independent of the
    // history/id machinery.
    let client = Arc::new(Recording::new(text_reply("hello back")));
    let mut run = HarnessRun::start(IDENTITY.to_string(), input("hi"), grant(client.clone()), Seed::default());
    let _ = drain_transcript(&mut run);
    run.join().expect("run must finish cleanly");

    let rendered = client.rendered();
    assert_eq!(rendered.len(), 1);
    let messages_region = rendered[0]
        .prefix
        .regions
        .get("messages")
        .expect("a messages region must have been rendered");
    let wrapped = format!("[{}]", String::from_utf8_lossy(messages_region));
    let parsed: serde_json::Value =
        serde_json::from_str(&wrapped).expect("real wire bytes must be valid JSON");
    let messages = parsed.as_array().expect("messages region is an array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
}

#[test]
fn the_fold_invariant_holds_for_a_completed_model_frame() {
    // harness-events.md §8: folding a model frame's `model_event`s through
    // `Accumulator` must reproduce the same text the harness actually saw
    // in its transcript.
    use harness::frame::{CoreEvent, Outcome};

    let client = Arc::new(Scripted::new(text_reply("fold me")));
    let mut run = HarnessRun::start(IDENTITY.to_string(), input("hi"), grant(client), Seed::default());
    let transcript = drain_transcript(&mut run);
    let frames = run.join().expect("run must finish cleanly").frames;

    let frame_id = frames
        .iter()
        .find_map(|frame| match frame {
            CoreEvent::FrameStart { frame, .. } => Some(frame.clone()),
            _ => None,
        })
        .expect("a model frame must have started");
    assert!(frames.iter().any(|frame| matches!(
        frame,
        CoreEvent::FrameStop { frame: id, outcome: Outcome::Ok } if *id == frame_id
    )));

    let mut accumulator = llm::Accumulator::new();
    for frame in &frames {
        if let CoreEvent::ModelEvent { frame: id, event } = frame
            && *id == frame_id
        {
            accumulator.push(event);
        }
    }
    let completion = accumulator.finish().expect("the folded frame must complete");

    let transcript_text: String = transcript
        .iter()
        .filter(|event| event["type"] == "block_delta" && event["delta"]["type"] == "text")
        .map(|event| event["delta"]["text"].as_str().unwrap_or_default())
        .collect();

    assert_eq!(completion.text(), transcript_text);
    assert_eq!(completion.text(), "fold me");
}

#[test]
fn turn_one_has_no_committed_history_and_still_produces_valid_messages() {
    // §4's default path: nothing was ever committed, so `ctx.history.read()`
    // must still be usable — it returns `[]`.
    let client = Arc::new(Scripted::new(text_reply("first turn")));
    let mut run = HarnessRun::start(IDENTITY.to_string(), input("hi"), grant(client.clone()), Seed::default());
    let _ = drain_transcript(&mut run);
    run.join().expect("turn 1 must not require a prior commit");

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 1);
}

#[test]
fn commit_is_callable_and_advances_history_within_one_run() {
    // A single `execute` that streams, commits the resulting call, then
    // reads history back and yields what it found — proving `commit` and
    // `history.read` are wired end to end. Persisting a commit *across*
    // separate harness runs is a workflow-layer concern (`crates/api`) and
    // isn't tested here — see `history.rs` for the two-run case, which seeds
    // the second run explicitly rather than relying on that spine.
    const COMMIT_THEN_READ: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...ctx.history.read(), input] });
    for await (const event of call) { /* drain */ }
    await ctx.commit(call);
    const history = ctx.history.read();
    yield { type: "unknown", raw: { historyLength: history.length } };
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("ok")));
    let mut run = HarnessRun::start(COMMIT_THEN_READ.to_string(), input("hi"), grant(client), Seed::default());
    let events = drain_transcript(&mut run);
    run.join().expect("commit must be callable when granted");

    assert_eq!(events.len(), 1);
    // The committed lineage covers the user turn plus the assistant's reply.
    assert_eq!(events[0]["raw"]["historyLength"], 2);
}

#[test]
fn commit_is_absent_not_thrown_when_not_granted() {
    const CHECKS_COMMIT: &str = r#"
export default {
  execute: async function* (ctx, input) {
    yield { type: "unknown", raw: { commitIsFunction: typeof ctx.commit === "function" } };
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut grant = grant(client);
    grant.commit_granted = false;

    let mut run = HarnessRun::start(CHECKS_COMMIT.to_string(), input("hi"), grant, Seed::default());
    let events = drain_transcript(&mut run);
    run.join().expect("a harness that never calls commit must still finish cleanly");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["raw"]["commitIsFunction"], false);
}

#[test]
fn a_tool_calling_harness_sees_both_failure_axes() {
    const TOOL_HARNESS: &str = r#"
export default {
  execute: async function* (ctx, input) {
    for (const tool of ["known", "unknown"]) {
      const result = ctx.tools.invoke(tool, {});
      if (result === undefined) {
        yield { type: "unknown", raw: { tool, offered: false } };
        continue;
      }
      const awaited = await result;
      yield { type: "tool_result", toolUseId: tool, content: awaited.content, isError: awaited.isError };
    }
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut g = grant(client);
    g.tools = vec![llm::ToolDef {
        name: "known".into(),
        description: "a tool that exists".into(),
        input_schema: serde_json::json!({}),
    }];
    g.tool_invoker = Some(Arc::new(|name, _input| {
        Box::pin(async move {
            if name == "known" {
                Ok(harness::mapping::ToolResult {
                    content: "42".into(),
                    is_error: false,
                })
            } else {
                Err("dispatch failed".into())
            }
        })
    }));

    let mut run = HarnessRun::start(TOOL_HARNESS.to_string(), input("hi"), g, Seed::default());
    let events = drain_transcript(&mut run);
    run.join().expect("run must finish");

    // Axis 1: a granted tool that ran and returned successfully.
    assert_eq!(events[0]["type"], "tool_result");
    assert_eq!(events[0]["content"], "42");
    assert_eq!(events[0]["isError"], false);

    // Axis 2: a tool that was never offered — "not offered" is a move the
    // loop doesn't have, not an exception to catch (types.d.ts).
    assert_eq!(events[1]["raw"]["offered"], false);
}

#[test]
fn concurrent_polling_of_one_call_is_a_clear_error_not_a_lost_stream() {
    // A harness that iterates a `Call` and reads `.completion` on it
    // *concurrently* (rather than sequentially, the documented pattern) must
    // not have the second poll misreport the stream as unknown just because
    // the first is mid-await.
    const RACES_ITS_OWN_CALL: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...ctx.history.read(), input] });
    const iterate = (async () => { for await (const _ of call) { /* drain */ } })();
    const completionRace = call.completion.catch((error) => ({ raced: true, name: error.name }));
    const [, completionResult] = await Promise.all([iterate, completionRace]);
    yield { type: "unknown", raw: { completionResult } };
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("racing")));
    let mut run = HarnessRun::start(RACES_ITS_OWN_CALL.to_string(), input("hi"), grant(client), Seed::default());
    let events = drain_transcript(&mut run);
    run.join().expect("racing a call's own completion must not crash the run");

    assert_eq!(events.len(), 1);
    // Whichever side loses the race gets a real rejection (not `undefined`,
    // not a silently empty result) — the two acceptable outcomes are "the
    // completion resolved fine" (the iterator happened to finish first) or
    // "it raced and got a named error" (StreamBusy caught it). Either is a
    // sound outcome; a lost/misreported stream is not.
    let raw = &events[0]["raw"]["completionResult"];
    assert!(
        raw.get("id").is_some() || raw["raced"] == true,
        "expected either a resolved completion or a caught race error, got {raw:?}"
    );
}

#[test]
fn a_harness_that_never_yields_is_terminated_and_the_run_ends() {
    const INFINITE_LOOP: &str = r#"
export default {
  execute: async function* () {
    while (true) { /* spin */ }
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let run = HarnessRun::start(INFINITE_LOOP.to_string(), input("hi"), grant(client), Seed::default());

    std::thread::sleep(Duration::from_millis(200));
    run.terminate();

    // The thread must actually finish — a runaway harness degrades only its
    // own run, not the process. `join` may report a Core error (the
    // terminated execution) or succeed with an empty frame log; the
    // assertion that matters is that it returns at all.
    let result = std::thread::spawn(move || run.join())
        .join()
        .expect("the harness thread must not itself panic");
    let _ = result;
}
