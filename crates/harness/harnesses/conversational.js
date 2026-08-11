// Identity, plus a commit — the loop a multi-turn conversation needs.
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
export default {
  async *execute(ctx, input) {
    const call = ctx.llm.stream({ messages: [...ctx.history.read(), input] });
    yield* call;
    // After the stream is drained, so the completion exists to adopt. The
    // call is passed whole — it already holds exactly what was sent and
    // exactly what came back (proposal.md §6.1).
    await ctx.commit(call);
  },
};
