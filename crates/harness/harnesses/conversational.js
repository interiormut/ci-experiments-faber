// Identity, plus a commit and a tool loop — what a multi-turn conversation
// with an environment needs.
//
// `identity.js` is the literal form of abstract.md §4's "default is
// identity" and commits nothing, which is correct for what it claims and
// wrong for a conversation: types.d.ts is explicit that history does not
// auto-advance in the current substrate, so "a harness that streams a call
// and never commits it leaves history.read() returning the same thing next
// time." Every turn would then start from an empty lineage.
//
// H4's answer is Core adopting the last request plus its completion by
// default, with commit demoted to the best-of-N override. Until that lands
// this file is the difference, and it should disappear when it does.
//
// The tool loop is here rather than in each harness for the same reason the
// tool surface is not written per harness: a granted tool the loop never
// invokes is a capability the model can see and cannot use, which is worse
// than not granting it. Only the *last* call is committed, and that is
// enough — each call carries every earlier turn by value, so the final one's
// turn list is the whole exchange, tool calls and results included.

/// How many times the model may call tools before the loop stops offering it
/// the chance. Not a safety boundary — the environment holds those — but a
/// loop with no bound is a loop that can spend a user's budget forever on a
/// command that keeps failing the same way.
const MAX_STEPS = 64;
async function generateTitle(ctx, input) {
  const history = ctx.history.read();
  if (history.length > 0) return null;

  if (!ctx.functions.available.includes("session.get_title")) return null;

  try {
    const currentTitle = await ctx.functions.invoke("session.get_title", null);
    if (currentTitle !== null) return null;

    const call = ctx.llm.stream({
      messages: [
        {
          role: "system",
          content: [
            {
              type: "text",
              text: "Generate a concise title for the user's prompt. Reply with only the title, with no quotes or explanation.",
            },
          ],
        },
        ...input,
      ],
      tools: [],
      maxTokens: 64,
    });

    for await (const _event of call) {
      // Drain the hidden title stream.
    }
    const completion = await call.completion;
    const title = (completion.message.content ?? [])
      .filter((block) => block.type === "text")
      .map((block) => block.text)
      .join("")
      .trim();

    if (title) {
      // The initial check can become stale while the hidden title request runs.
      // Do not replace a title another run or the user set in the meantime.
      const latestTitle = await ctx.functions.invoke("session.get_title", null);
      if (latestTitle !== null) return null;
      await ctx.functions.invoke("session.set_title", title);
      return title;
    }
  } catch {
    // Title generation is best effort and must not affect the main response.
  }
  return null;
}

export default {
  async *execute(ctx, input) {
    const messages = [...ctx.history.read(), ...input];
    let call = null;

    // Start title generation in parallel. It must not delay the first visible
    // model event, but the promise is awaited after the response so the
    // isolate stays alive long enough to persist the title.
    const titlePromise = generateTitle(ctx, input);

    for (let step = 0; step < MAX_STEPS; step += 1) {
      call = ctx.llm.stream({ messages });
      yield* call;

      const completion = await call.completion;
      const content = completion.message.content ?? [];
      const calls = content.filter((block) => block.type === "tool_use");
      if (calls.length === 0) {
        break;
      }

      // The assistant's turn goes back exactly as it came, tool_use blocks
      // and all: a tool_result whose tool_use is missing or edited is a
      // malformed request, and the pairing is checked before anything is
      // sent.
      messages.push(completion.message);

      const results = [];
      for (const use of calls) {
        yield { type: "tool_call", id: use.id, name: use.name, input: use.input };

        // `undefined` means the tool was never granted — a move this loop
        // does not have. Saying so as a tool_result keeps the pairing intact;
        // dropping it would orphan the tool_use and refuse the next request.
        const invocation = ctx.tools.invoke(use.name, use.input);
        const result =
          invocation === undefined
            ? { content: `\`${use.name}\` is not a tool this run was granted`, isError: true }
            : await invocation;

        yield {
          type: "tool_result",
          id: use.id,
          name: use.name,
          content: result.content,
          isError: result.isError,
        };
        results.push({
          type: "tool_result",
          toolUseId: use.id,
          content: result.content,
          isError: result.isError,
        });
      }

      messages.push({ role: "user", content: results });
    }

    // After the loop, so what is adopted is the call the conversation
    // actually ended on. The call is passed whole — it already holds exactly
    // what was sent and exactly what came back (proposal.md §6.1).
    if (call !== null) {
      await ctx.commit(call);
    }

    const title = await titlePromise;
    if (title !== null) {
      yield { type: "session_title", title };
    }
  },
};
