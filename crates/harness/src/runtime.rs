//! Drives one harness run to completion: boots an isolate on a dedicated
//! thread, injects `ctx`, evaluates the harness's `execute`, and surfaces the
//! two logs (H2) back to the caller.
//!
//! One thread per run, each with its own single-threaded tokio runtime and
//! `LocalSet` — `JsRuntime` is `!Send`, and a dedicated thread is also the
//! isolation boundary §7 asks for: a runaway harness is killed with
//! `terminate_execution()` and degrades only its own run.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::{JsRuntime, ModuleSpecifier, RuntimeOptions};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::frame::CoreEvent;
use crate::loader::{BOOTSTRAP_SPECIFIER, CONTEXT_SPECIFIER, HARNESS_SPECIFIER, HarnessLoader};
use crate::mapping;
use crate::state::{Grant, HarnessState, Seed};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("failed to start the harness thread's runtime: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("harness input did not encode to JSON: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("module specifier was invalid: {0}")]
    Specifier(#[from] url::ParseError),
    #[error(transparent)]
    Core(#[from] deno_core::error::CoreError),
    #[error("the harness thread panicked")]
    ThreadPanicked,
}

/// A running (or finished) harness. `transcript` streams live; the frame log
/// and any run-level error are only available after [`HarnessRun::join`].
pub struct HarnessRun {
    pub transcript: UnboundedReceiver<serde_json::Value>,
    isolate_handle: Option<deno_core::v8::IsolateHandle>,
    thread: std::thread::JoinHandle<Result<Vec<CoreEvent>, RunError>>,
}

impl HarnessRun {
    /// Boots an isolate on a new thread and starts running `harness_source`'s
    /// default export against `input`, under `grant`. `seed` is what a prior
    /// run committed — `Seed::default()` for a fresh conversation's first
    /// turn, since nothing in the isolate survives between runs.
    pub fn start(
        harness_source: String,
        input: llm::Message,
        grant: Grant,
        seed: Seed,
    ) -> HarnessRun {
        let (transcript_tx, transcript_rx) = tokio::sync::mpsc::unbounded_channel();
        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
        let (handle_tx, handle_rx) =
            std::sync::mpsc::channel::<deno_core::v8::IsolateHandle>();

        let thread = std::thread::Builder::new()
            .name("harness".into())
            .spawn(move || -> Result<Vec<CoreEvent>, RunError> {
                let tokio_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let local = tokio::task::LocalSet::new();

                let result: Result<(), RunError> = local.block_on(&tokio_rt, async move {
                    let bootstrap_source = build_bootstrap(&input)?;
                    let loader = HarnessLoader { harness_source };

                    let mut runtime = JsRuntime::new(RuntimeOptions {
                        module_loader: Some(Rc::new(loader)),
                        extensions: vec![crate::ops::faber::init()],
                        ..Default::default()
                    });

                    {
                        let op_state = runtime.op_state();
                        op_state
                            .borrow_mut()
                            .put(Rc::new(RefCell::new(HarnessState::new(
                                grant,
                                seed,
                                transcript_tx,
                                frames_tx,
                            ))));
                    }

                    // Grabbed before the run so the caller can terminate a
                    // runaway harness regardless of what it does next.
                    let _ = handle_tx.send(runtime.v8_isolate().thread_safe_handle());

                    let main = ModuleSpecifier::parse(BOOTSTRAP_SPECIFIER)?;
                    let mod_id = runtime
                        .load_main_es_module_from_code(&main, bootstrap_source)
                        .await?;
                    let evaluated = runtime.mod_evaluate(mod_id);
                    runtime.run_event_loop(Default::default()).await?;
                    evaluated.await?;
                    Ok(())
                });
                result?;

                let mut frames = Vec::new();
                while let Ok(frame) = frames_rx.try_recv() {
                    frames.push(frame);
                }
                Ok(frames)
            })
            .expect("spawning the harness thread must not fail");

        let isolate_handle = handle_rx.recv().ok();

        HarnessRun {
            transcript: transcript_rx,
            isolate_handle,
            thread,
        }
    }

    /// Kills the isolate. Safe to call from any thread, at any time — the
    /// backstop for a harness that never yields control back
    /// (`history-abstract.md` H9.3 notes this is the mechanism, not a signal
    /// threaded into JS).
    pub fn terminate(&self) {
        if let Some(handle) = &self.isolate_handle {
            handle.terminate_execution();
        }
    }

    /// Blocks until the run finishes, returning the frame log or the error
    /// that ended it (a thrown/rejected top-level error, an op rejection
    /// that propagated, or termination).
    pub fn join(self) -> Result<Vec<CoreEvent>, RunError> {
        self.thread.join().map_err(|_| RunError::ThreadPanicked)?
    }
}

fn build_bootstrap(input: &llm::Message) -> Result<String, RunError> {
    let wire_input = mapping::Message::from(input);
    let json = serde_json::to_string(&wire_input)?;
    Ok(format!(
        r#"import {{ buildContext }} from "{CONTEXT_SPECIFIER}";
import harness from "{HARNESS_SPECIFIER}";

const ctx = buildContext();
const input = {json};

for await (const event of harness.execute(ctx, input)) {{
  Deno.core.ops.op_yield(event);
}}
"#
    ))
}
