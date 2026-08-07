// The bare harness: passes its input straight to the model, appended to
// whatever history.read() hands back. §4's "default is identity" — this is
// the literal claim made paste-able as code.
export default {
  execute: (ctx, input) =>
    ctx.llm.stream({ messages: [...ctx.history.read(), input] }),
};
