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

use crate::frame::{CoreEvent, FrameId};
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

/// Everything a finished run leaves behind, beyond what already streamed.
pub struct RunOutcome {
    /// What happened, in order (`harness-events.md` §4).
    pub frames: Vec<CoreEvent>,
    /// The canonical lineage as this run left it — what seeds turn N+1
    /// (`history-abstract.md` H7). Equal to the run's own [`Seed`] when the
    /// harness never committed, since nothing moved the lineage.
    pub committed: Seed,
    /// The model frame whose commit produced [`Self::committed`], so a caller
    /// writing a spine can point it at the matching exchange. `None` when the
    /// harness never called `commit` — there is then no position to record.
    pub committed_frame: Option<FrameId>,
}

/// A running (or finished) harness. `transcript` streams live; the frame log
/// and any run-level error are only available after [`HarnessRun::join`].
pub struct HarnessRun {
    pub transcript: UnboundedReceiver<serde_json::Value>,
    isolate_handle: Option<deno_core::v8::IsolateHandle>,
    thread: std::thread::JoinHandle<Result<RunOutcome, RunError>>,
}

/// A standalone kill switch for one run's isolate, handed out by
/// [`HarnessRun::terminator`]. Terminating a run that has already finished is
/// a no-op, so a stale one is harmless to hold.
#[derive(Clone)]
pub struct Terminator(Option<deno_core::v8::IsolateHandle>);

impl Terminator {
    pub fn terminate(&self) {
        if let Some(handle) = &self.0 {
            handle.terminate_execution();
        }
    }
}

impl HarnessRun {
    /// Boots an isolate on a new thread and starts running `harness_source`'s
    /// default export against `input`, under `grant`. `seed` is what a prior
    /// run committed — `Seed::default()` for a fresh conversation's first
    /// turn, since nothing in the isolate survives between runs.
    ///
    /// `input` is the turn's messages, plural. A turn is usually one message
    /// and often will be, but not always: a consumer that has something to
    /// tell the model alongside the user's own words — that an environment was
    /// added, say — has to put it *in* the conversation, and the alternative
    /// is editing the system prompt, which is prefix mutation and invalidates
    /// everything cached behind it.
    pub fn start(
        harness_source: String,
        input: Vec<llm::Message>,
        grant: Grant,
        seed: Seed,
    ) -> HarnessRun {
        let (transcript_tx, transcript_rx) = tokio::sync::mpsc::unbounded_channel();
        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
        let (handle_tx, handle_rx) = std::sync::mpsc::channel::<deno_core::v8::IsolateHandle>();

        let thread = std::thread::Builder::new()
            .name("harness".into())
            .spawn(move || -> Result<RunOutcome, RunError> {
                let tokio_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let local = tokio::task::LocalSet::new();

                // The lineage has to outlive the isolate that produced it:
                // `HarnessState` is created inside the async block (it moves
                // `grant` and `seed`) but is read after the event loop has
                // stopped. Everything here stays on this one thread, so an
                // `Rc` is the whole of the machinery required.
                let state_slot: Rc<RefCell<Option<Rc<RefCell<HarnessState>>>>> =
                    Rc::new(RefCell::new(None));
                let state_out = Rc::clone(&state_slot);

                let result: Result<(), RunError> = local.block_on(&tokio_rt, async move {
                    let bootstrap_source = build_bootstrap(&input)?;
                    let loader = HarnessLoader { harness_source };

                    let mut runtime = JsRuntime::new(RuntimeOptions {
                        module_loader: Some(Rc::new(loader)),
                        extensions: vec![crate::ops::faber::init()],
                        ..Default::default()
                    });

                    {
                        let harness = Rc::new(RefCell::new(HarnessState::new(
                            grant,
                            seed,
                            transcript_tx,
                            frames_tx,
                        )));
                        *state_out.borrow_mut() = Some(Rc::clone(&harness));
                        runtime.op_state().borrow_mut().put(harness);
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

                let (committed, committed_frame) = match state_slot.borrow().as_ref() {
                    Some(harness) => {
                        let harness = harness.borrow();
                        (
                            Seed {
                                provider: harness.grant.client.provider().to_string(),
                                model: harness.grant.model.clone(),
                                messages: harness.lineage_iter().cloned().collect(),
                                options: harness.baseline.clone(),
                            },
                            harness.committed_frame.clone(),
                        )
                    }
                    // Unreachable in practice — the slot is filled before the
                    // module is ever loaded — but a default keeps a future
                    // reordering from turning this into a panic.
                    None => (Seed::default(), None),
                };

                Ok(RunOutcome {
                    frames,
                    committed,
                    committed_frame,
                })
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

    /// The same kill, detached from the run handle so it can be held by
    /// something else while the run is being consumed.
    ///
    /// [`Self::terminate`] needs `&self`, and a caller that streams the
    /// transcript and then `join`s has given the handle away by the time it
    /// would want to use it. A watchdog waiting out an interrupt's grace
    /// period is exactly that caller.
    pub fn terminator(&self) -> Terminator {
        Terminator(self.isolate_handle.clone())
    }

    /// Blocks until the run finishes, returning what it left behind or the
    /// error that ended it (a thrown/rejected top-level error, an op
    /// rejection that propagated, or termination).
    ///
    /// A run that ends in `Err` yields no [`RunOutcome`] at all, so a failed
    /// run records no exchanges and moves no lineage — deliberately: a
    /// lineage assembled from a run that did not finish is exactly the
    /// truncation `incomplete_completion` exists to keep out of history.
    pub fn join(self) -> Result<RunOutcome, RunError> {
        self.thread.join().map_err(|_| RunError::ThreadPanicked)?
    }
}

fn build_bootstrap(input: &[llm::Message]) -> Result<String, RunError> {
    let wire_input: Vec<mapping::Message> = input.iter().map(mapping::Message::from).collect();
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
