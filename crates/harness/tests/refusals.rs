//! One end-to-end test per surviving §7 row (`proposal.md`) — everything
//! `op_llm_stream_open`/`op_commit` can refuse a request for, reached through
//! a real harness rather than unit-tested against `validate::check` directly.
//!
//! Not covered here:
//! - `scaffold_mismatch { component: "model" }` — unreachable within one run
//!   (one client, hence one model, per run); covered as a unit test in
//!   `crates/harness/src/validate.rs`.
//! - `tool_removal` — proposal §5 (mid-conversation tool changes) is
//!   deferred, so there is nothing yet that could orphan a removal.

mod support;

use std::sync::Arc;
use std::time::Duration;

use harness::{HarnessRun, Seed};
use support::{
    Failing, Scripted, drain_and_join_with_timeout, drain_transcript, grant, input, text_reply,
};

/// Runs `harness_source` and returns the single `{type:"unknown", raw:{...}}`
/// event every test in this file yields, after catching (or not) the
/// refusal it's built to provoke.
fn run(harness_source: &str, client: Arc<dyn llm::ModelClient>) -> serde_json::Value {
    let mut run = HarnessRun::start(
        harness_source.to_string(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let mut events = drain_transcript(&mut run);
    run.join()
        .expect("the harness itself must not error — every refusal here is caught in JS");
    assert_eq!(
        events.len(),
        1,
        "each test yields exactly one summary event"
    );
    events.remove(0)["raw"].clone()
}

/// Wraps a body that attempts one `ctx.llm.stream(...)` call (synchronously
/// throwing on an open-time refusal) and reports the caught error's `kind`,
/// or `null` if nothing was thrown.
fn catches(body_stream_expr: &str) -> String {
    format!(
        r#"
export default {{
  execute: async function* (ctx, input) {{
    try {{
      {body_stream_expr}
      yield {{ type: "unknown", raw: {{ kind: null }} }};
    }} catch (e) {{
      yield {{ type: "unknown", raw: {{ kind: e.kind }} }};
    }}
  }}
}};
"#
    )
}

#[test]
fn ref_not_prefix_on_reordered_ids() {
    let client = Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("unused"),
    ]));
    let source = catches(
        r#"
      const call1 = ctx.llm.stream({ messages: [...input] });
      for await (const e of call1) {}
      await ctx.commit(call1);
      const h = ctx.history.read();
      const call2 = ctx.llm.stream({ messages: [h[1], h[0]] });
      for await (const e of call2) {}
    "#,
    );
    let raw = run(&source, client);
    assert_eq!(raw["kind"], "ref_not_prefix");
}

#[test]
fn ref_not_prefix_on_an_unknown_id() {
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let source = catches(r#"ctx.llm.stream({ messages: [{ id: "m99" }, ...input] });"#);
    let raw = run(&source, client);
    assert_eq!(raw["kind"], "ref_not_prefix");
}

#[test]
fn ref_content_mismatch_on_an_id_retaining_edit() {
    let client = Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("unused"),
    ]));
    let source = catches(
        r#"
      const call1 = ctx.llm.stream({ messages: [...input] });
      for await (const e of call1) {}
      await ctx.commit(call1);
      const h = ctx.history.read();
      // The idiomatic mistake: a spread copy keeps the id.
      const edited = { ...h[1], content: [{ type: "text", text: "not what was stored" }] };
      const call2 = ctx.llm.stream({ messages: [h[0], edited] });
      for await (const e of call2) {}
    "#,
    );
    let raw = run(&source, client);
    assert_eq!(raw["kind"], "ref_content_mismatch");
}

#[test]
fn malformed_turns_on_an_orphaned_tool_result() {
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let source = catches(
        r#"
      ctx.llm.stream({
        messages: [
          { role: "user", content: [{ type: "tool_result", toolUseId: "t1", content: "42", isError: false }] },
        ],
      });
    "#,
    );
    let raw = run(&source, client);
    assert_eq!(raw["kind"], "malformed_turns");
}

#[test]
fn malformed_turns_on_consecutive_system_messages_after_the_head() {
    let client = Arc::new(Scripted::new(text_reply("unused")));
    let source = catches(
        r#"
      ctx.llm.stream({
        messages: [
          { role: "system", content: [{ type: "text", text: "sys" }] },
          { role: "user", content: [{ type: "text", text: "hi" }] },
          { role: "system", content: [{ type: "text", text: "mid1" }] },
          { role: "system", content: [{ type: "text", text: "mid2" }] },
        ],
      });
    "#,
    );
    let raw = run(&source, client);
    assert_eq!(raw["kind"], "malformed_turns");
}

#[test]
fn scaffold_mismatch_on_reordered_tools() {
    let client = Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("unused"),
    ]));
    let mut g = grant(client.clone());
    g.tools = vec![
        llm::ToolDef {
            name: "a".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        },
        llm::ToolDef {
            name: "b".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        },
    ];
    let source = catches(
        r#"
      const call1 = ctx.llm.stream({ messages: [...input] });
      for await (const e of call1) {}
      await ctx.commit(call1);
      const h = ctx.history.read();
      const call2 = ctx.llm.stream({
        messages: [h[0], h[1], ...input],
        tools: [
          { name: "b", description: "", inputSchema: {} },
          { name: "a", description: "", inputSchema: {} },
        ],
      });
      for await (const e of call2) {}
    "#,
    );
    let mut run = HarnessRun::start(source.to_string(), input("hi"), g, Seed::default());
    let mut events = drain_transcript(&mut run);
    run.join().expect("the refusal is caught in JS");
    let raw = events.remove(0)["raw"].clone();
    assert_eq!(raw["kind"], "scaffold_mismatch");
}

#[test]
fn scaffold_mismatch_on_a_partially_referenced_system_head() {
    let client = Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("unused"),
    ]));
    let source = catches(
        r#"
      const call1 = ctx.llm.stream({
        messages: [
          { role: "system", content: [{ type: "text", text: "sys1" }] },
          { role: "system", content: [{ type: "text", text: "sys2" }] },
          ...input,
        ],
      });
      for await (const e of call1) {}
      await ctx.commit(call1);
      const h = ctx.history.read();
      // References only the first of the two-message system head, then goes
      // straight to by-value content — the committed lineage's head no
      // longer matches what this request's own head resolves to.
      const call2 = ctx.llm.stream({ messages: [{ id: h[0].id }, ...input] });
      for await (const e of call2) {}
    "#,
    );
    let raw = run(&source, client);
    assert_eq!(raw["kind"], "scaffold_mismatch");
}

#[test]
fn incomplete_completion_is_refused_by_default_and_accepted_as_partial() {
    let client: Arc<dyn llm::ModelClient> = Arc::new(Failing);
    let source = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...input] });
    try {
      for await (const event of call) { /* drain */ }
    } catch (e) { /* the stream itself is expected to fail */ }

    let firstKind = null;
    try {
      await ctx.commit(call);
    } catch (e) {
      firstKind = e.kind;
    }

    await ctx.commit(call, { partial: true });
    const history = ctx.history.read();
    yield { type: "unknown", raw: { firstKind, historyLength: history.length } };
  }
};
"#;
    let raw = run(source, client);
    assert_eq!(raw["firstKind"], "incomplete_completion");
    assert_eq!(raw["historyLength"], 2);
}

/// Regression: `op_commit`'s drain loop used to swallow whatever error
/// `advance` returned and loop again. For a call whose slot had already been
/// removed by an earlier commit — which is exactly the state a second
/// `commit(call)` finds — `advance` returns `UnknownStream` without ever
/// reaching an `.await`, so the loop never yielded and this hung the
/// isolate's event loop rather than failing. It must instead reject
/// immediately, promptly enough that the timeout below is never close.
#[test]
fn committing_the_same_call_twice_is_a_clear_error_not_a_hang() {
    const DOUBLE_COMMIT: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...input] });
    for await (const event of call) { /* drain */ }
    await ctx.commit(call);

    let secondName = null;
    try {
      await ctx.commit(call);
    } catch (e) {
      secondName = e.name;
    }
    yield { type: "unknown", raw: { secondName } };
  }
};
"#;
    let client = Arc::new(Scripted::new(text_reply("ok")));
    let run = HarnessRun::start(
        DOUBLE_COMMIT.to_string(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let (mut events, _) = drain_and_join_with_timeout(run, Duration::from_secs(5));

    assert_eq!(events.len(), 1);
    let raw = events.remove(0)["raw"].clone();
    assert_eq!(raw["secondName"], "RangeError");
}
