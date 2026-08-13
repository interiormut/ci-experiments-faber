//! Runs a harness: a piece of JavaScript owning the loop, executing inside a
//! `deno_core` isolate with a capability object injected as `ctx`.

mod canonical;
pub mod error;
pub mod frame;
pub mod interrupt;
mod loader;
pub mod mapping;
mod ops;
pub mod runtime;
mod scaffold;
pub mod state;
pub mod tools;
mod validate;

pub use interrupt::{Interrupt, Interrupter, interrupt};
pub use runtime::{HarnessRun, RunError, RunOutcome, Terminator};
pub use state::{Baseline, Grant, Seed};
pub use tools::Surface;

/// The bare harness: input straight to the model, appended to history.
/// Commits nothing, so it does not advance the lineage.
pub const IDENTITY: &str = include_str!("../harnesses/identity.js");

/// [`IDENTITY`] plus a `commit`, which is what a multi-turn conversation
/// needs until history auto-advances (`history-abstract.md` H4).
pub const CONVERSATIONAL: &str = include_str!("../harnesses/conversational.js");

/// Selects a harness for a model's `family`.
///
/// The fallback is load-bearing rather than a convenience (`a.md`): model
/// identity has no external authority — `family` is a user-supplied string,
/// and forks, fine-tunes, and unknown models will not match anything. Falling
/// through to a general-purpose loop is what keeps an unrecognized model
/// degraded rather than broken.
///
/// Nothing is family-specific yet, so every input resolves to
/// [`CONVERSATIONAL`]. The signature is the point: it is where per-family
/// harnesses attach without any caller changing.
pub fn harness_for(_family: Option<&str>) -> &'static str {
    CONVERSATIONAL
}
