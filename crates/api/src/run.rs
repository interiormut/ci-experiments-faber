//! Driving a harness run from the API, and getting what it produced onto disk
//! and out to a subscriber.
//!
//! Three things happen here that the rest of `crates/api` does not do:
//!
//! - A [`llm::ModelClient`] is constructed from a user's model row and their
//!   decrypted key ([`build_client`]).
//! - A harness executes in-process ([`spawn_run`]), streaming what it yields
//!   into both the `transcript` table and a `broadcast` channel.
//! - The two logs `history-abstract.md` H2 keeps separate are written from
//!   their two separate sources: the transcript from what the harness yielded,
//!   the exchange from the frame log Core recorded at the capability boundary.
//!   Neither derives the other, and the harness cannot forge the second.
//!
//! Cross-turn history (H7) closes here too: a run's committed lineage is
//! serialized into a `blob`, named by the `exchange` its commit came from, and
//! reached again on the next turn through the thread's `spine`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use diesel::{ExpressionMethods, JoinOnDsl, QueryDsl};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use harness::frame::{CoreEvent, FrameDetail, FrameId, Outcome};
use harness::{Grant, HarnessRun, RunOutcome, Seed};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    compact::{Compactor, KIND_MESSAGE},
    db::DbPool,
    error::{ApiResult, AppError},
    models::{
        blob::NewBlob,
        exchange::NewExchange,
        model_config::{ModelConfig, Wire},
        now_epoch,
        spine::NewSpine,
        transcript::NewTranscript,
    },
    schema::{blob, exchange, run, spine, thread},
    state::AppState,
};

/// How many events a subscriber may fall behind before it is told to re-sync.
///
/// A slow client must never stall the run that is feeding it, so the channel
/// drops rather than blocks; the subscriber sees `Lagged` and refetches
/// through `GET /api/runs/{id}/transcript`, which is durable and complete.
const BROADCAST_CAPACITY: usize = 1024;

/// Live subscribers, keyed by session.
///
/// In-process only: a subscriber reaches a run just when it connected to the
/// instance that owns it. With one instance that is total; with more it is
/// not, and the fix is a shared bus rather than a bigger map.
pub type RunRegistry = Arc<RwLock<HashMap<Uuid, SessionChannel>>>;

/// One session's channel, and how many runs are still publishing to it.
///
/// The count is what makes an entry safe to drop. Removing a channel a run
/// still holds does not stop that run — it keeps publishing into its own
/// clone — but it does route the *next* subscriber to a different channel,
/// which would show an empty stream while a run was plainly in progress.
pub struct SessionChannel {
    sender: broadcast::Sender<StreamEvent>,
    active_runs: usize,
}

impl SessionChannel {
    fn new() -> Self {
        Self {
            sender: broadcast::channel(BROADCAST_CAPACITY).0,
            active_runs: 0,
        }
    }

    /// Nothing is publishing and nobody is listening.
    fn is_inert(&self) -> bool {
        self.active_runs == 0 && self.sender.receiver_count() == 0
    }
}

/// One event on the wire to a subscriber.
///
/// `(run_id, seq)` together are the cursor, because `transcript.seq` is unique
/// per *run* (`UNIQUE (run_id, seq)`), not per session — a session-scoped
/// stream carries several runs' worth of independently-numbered events.
#[derive(Clone, Debug, Serialize)]
pub struct StreamEvent {
    pub run_id: Uuid,
    pub seq: i64,
    /// Free-form, deliberately not an enum on either side
    /// (`history-abstract.md` H8.7). Harness events carry their own `type`;
    /// this layer adds `input` and the two terminal markers.
    pub kind: String,
    pub payload: Value,
}

/// Terminal marker: the run finished and nothing more will arrive for it.
pub const KIND_RUN_END: &str = "run_end";
/// Terminal marker: the run ended in a failure the harness did not handle.
pub const KIND_RUN_ERROR: &str = "run_error";
/// The user's own turn. Not harness-yielded, but H2 makes the transcript the
/// user-facing conversation, and a conversation missing one side of itself is
/// not one.
pub const KIND_INPUT: &str = "input";

/// The `transcript.seq` the user's message occupies. Harness output starts
/// at 1.
pub const INPUT_SEQ: i64 = 0;

// ---------------------------------------------------------------------------
// Model clients
// ---------------------------------------------------------------------------

/// Builds a client for a user's model row.
///
/// `http: None` is deliberate. `AppState::http` carries a 10-second total
/// request timeout, and both provider modules warn that a total timeout cuts a
/// long generation off mid-answer; `crates/llm` builds its own with a connect
/// and a read timeout instead, which bounds a stalled connection without
/// bounding the generation.
pub fn build_client(config: &ModelConfig, api_key: String) -> ApiResult<Arc<dyn llm::ModelClient>> {
    let base_url = url::Url::parse(&config.base_url)
        .map_err(|_| AppError::BadRequest(format!("model {} has an invalid base_url", config.alias)))?;
    let key = secrecy::SecretString::from(api_key);

    let wire = Wire::from_db(&config.wire).ok_or_else(|| {
        AppError::BadRequest(format!("model {} has an unknown wire", config.alias))
    })?;

    let client: Arc<dyn llm::ModelClient> = match wire {
        Wire::Anthropic => Arc::new(
            llm::anthropic::Anthropic::new(llm::anthropic::Config {
                api_key: key,
                base_url: Some(base_url),
                betas: Vec::new(),
                http: None,
            })
            .map_err(|error| {
                tracing::error!(error = %error, "failed to build anthropic client");
                AppError::Internal
            })?,
        ),
        Wire::Openai => Arc::new(
            llm::openai::OpenAI::new(llm::openai::Config {
                api_key: key,
                base_url: Some(base_url),
                http: None,
            })
            .map_err(|error| {
                tracing::error!(error = %error, "failed to build openai client");
                AppError::Internal
            })?,
        ),
    };

    Ok(client)
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// The thread's canonical lineage, for the next run to start from.
///
/// Walks the last `spine` position to its exchange and reads the lineage blob
/// that exchange's commit produced. A thread with no spine, or whose newest
/// position predates this mechanism, yields `Seed::default()` — turn one,
/// which is also the correct answer for a lineage that cannot be recovered:
/// starting fresh is a visible loss of context, whereas a half-reconstructed
/// lineage is a silent one.
pub async fn load_seed(conn: &mut AsyncPgConnection, thread_id: Uuid) -> ApiResult<Seed> {
    let digest: Option<Vec<u8>> = spine::table
        .inner_join(exchange::table.on(exchange::id.eq(spine::exchange_id)))
        .filter(spine::thread_id.eq(thread_id))
        .order(spine::seq.desc())
        .select(exchange::canonical_blob_digest)
        .first::<Option<Vec<u8>>>(conn)
        .await
        .optional_row()
        .map_err(|err| AppError::db(err, "run.load_seed.spine_tail"))?
        .flatten();

    let Some(digest) = digest else {
        return Ok(Seed::default());
    };

    let data: Option<Vec<u8>> = blob::table
        .filter(blob::digest.eq(&digest))
        .select(blob::data)
        .first::<Option<Vec<u8>>>(conn)
        .await
        .optional_row()
        .map_err(|err| AppError::db(err, "run.load_seed.blob"))?
        .flatten();

    let Some(data) = data else {
        tracing::warn!(%thread_id, "canonical lineage blob is missing its data; seeding empty");
        return Ok(Seed::default());
    };

    match serde_json::from_slice::<Seed>(&data) {
        Ok(seed) => Ok(seed),
        Err(error) => {
            tracing::error!(%thread_id, error = %error, "canonical lineage failed to decode; seeding empty");
            Ok(Seed::default())
        }
    }
}

/// `diesel::result::Error::NotFound` means "no row", which is not a failure
/// for any of the tail lookups above.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>, diesel::result::Error>;
}

impl<T> OptionalRow<T> for Result<T, diesel::result::Error> {
    fn optional_row(self) -> Result<Option<T>, diesel::result::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(diesel::result::Error::NotFound) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// Everything one run needs, assembled by the route before it detaches.
pub struct RunRequest {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub thread_id: Uuid,
    pub config: ModelConfig,
    pub api_key: String,
    pub input: llm::Message,
}

/// Claims the session's channel for a run that is about to start.
///
/// Called before the POST returns, so a subscriber arriving in the window
/// between the response and the harness's first event still finds the channel
/// the run will publish to. Paired with [`close_run`].
pub fn open_run(registry: &RunRegistry, session_id: Uuid) -> broadcast::Sender<StreamEvent> {
    let mut guard = registry.write().expect("run registry lock poisoned");
    guard.retain(|id, channel| *id == session_id || !channel.is_inert());
    let channel = guard.entry(session_id).or_insert_with(SessionChannel::new);
    channel.active_runs += 1;
    channel.sender.clone()
}

/// Releases a run's claim, dropping the channel once nothing needs it.
pub fn close_run(registry: &RunRegistry, session_id: Uuid) {
    let mut guard = registry.write().expect("run registry lock poisoned");
    if let Some(channel) = guard.get_mut(&session_id) {
        channel.active_runs = channel.active_runs.saturating_sub(1);
        if channel.is_inert() {
            guard.remove(&session_id);
        }
    }
}

/// Subscribes to a session's live events.
///
/// Creates the channel if there isn't one, rather than handing back an empty
/// stream: opening the stream *before* sending the first message is the normal
/// order for a UI, and a subscriber that connected first must not be the one
/// that misses the run.
pub fn subscribe(registry: &RunRegistry, session_id: Uuid) -> broadcast::Receiver<StreamEvent> {
    let mut guard = registry.write().expect("run registry lock poisoned");
    guard.retain(|id, channel| *id == session_id || !channel.is_inert());
    guard
        .entry(session_id)
        .or_insert_with(SessionChannel::new)
        .sender
        .subscribe()
}

/// Runs a harness to completion in the background.
///
/// Detached on purpose: the POST that started it has already returned, and the
/// run must survive both that response and any subscriber disconnecting. The
/// only way to observe it afterwards is the transcript — live or persisted.
/// `sender` is the one [`open_run`] already handed the caller, so the claim is
/// taken exactly once and before this task is scheduled.
pub fn spawn_run(state: AppState, sender: broadcast::Sender<StreamEvent>, request: RunRequest) {
    tokio::spawn(async move {
        let session_id = request.session_id;
        let run_id = request.run_id;

        let result = execute(&state, &sender, request).await;

        let terminal = match result {
            Ok(()) => StreamEvent {
                run_id,
                seq: -1,
                kind: KIND_RUN_END.to_owned(),
                payload: Value::Null,
            },
            Err(error) => {
                tracing::error!(%run_id, error = %error, "harness run failed");
                StreamEvent {
                    run_id,
                    seq: -1,
                    kind: KIND_RUN_ERROR.to_owned(),
                    payload: json!({ "message": error.to_string() }),
                }
            }
        };
        let _ = sender.send(terminal);

        // Stamped whether the run succeeded or failed — `completed_at` records
        // that the run is over, not that it went well, and it is the durable
        // terminator a client that reconnects after the fact reads. (The two
        // markers above are live-only: they are stream control, not something
        // the harness yielded, and H2 keeps the transcript to the latter.)
        if let Err(error) = mark_run_complete(&state.db, run_id).await {
            tracing::error!(%run_id, error = %error, "failed to stamp run completion");
        }

        // Dropped before the claim is released, so the count is the only thing
        // keeping the entry alive by the time `close_run` inspects it.
        drop(sender);
        close_run(&state.runs, session_id);
    });
}

async fn execute(
    state: &AppState,
    sender: &broadcast::Sender<StreamEvent>,
    request: RunRequest,
) -> ApiResult<()> {
    let RunRequest {
        run_id,
        thread_id,
        config,
        api_key,
        input,
        ..
    } = request;

    let client = build_client(&config, api_key)?;

    let seed = {
        let mut conn = state.db.get().await?;
        load_seed(&mut conn, thread_id).await?
    };

    let grant = Grant {
        client,
        model: config.wire_id.clone(),
        // No tool surface in the API yet. `abstract.md` §4's control by
        // subtraction means this is a loop with fewer moves, not an error
        // condition: an ungranted tool is simply absent from `ctx`.
        tools: Vec::new(),
        tool_invoker: None,
        commit_granted: true,
    };

    let started_at = now_epoch();
    let mut run = HarnessRun::start(
        harness::harness_for(config.family.as_deref()).to_owned(),
        input,
        grant,
        seed,
    );

    // Harness output starts after the user's own turn.
    //
    // Every event is published live and given a `seq`, so the SSE resume
    // cursor stays gap-free; only what [`Compactor`] calls durable is
    // written. The persisted rows therefore have holes in `seq`, which the
    // `seq > after_seq` reads and the client's dedupe-by-seq both already
    // tolerate.
    let mut seq = INPUT_SEQ + 1;
    let mut compactor = Compactor::new();
    while let Some(event) = run.transcript.recv().await {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();

        let folded = compactor.push(&kind, &event);

        if let Some(message) = folded.flushed {
            publish(state, sender, run_id, &mut seq, KIND_MESSAGE.to_owned(), message, true).await?;
        }
        publish(state, sender, run_id, &mut seq, kind, event, folded.persist_raw).await?;
        if let Some(message) = folded.completed {
            publish(state, sender, run_id, &mut seq, KIND_MESSAGE.to_owned(), message, true).await?;
        }
    }

    // Before the join, not after: a harness that ends in error never reaches
    // `record_history`, and the text it did stream is still what the user saw.
    if let Some(message) = compactor.finish() {
        publish(state, sender, run_id, &mut seq, KIND_MESSAGE.to_owned(), message, true).await?;
    }

    // `join` blocks on an OS thread handle, which must not happen on the async
    // runtime's worker.
    let outcome = tokio::task::spawn_blocking(move || run.join())
        .await
        .map_err(|error| {
            tracing::error!(%run_id, error = %error, "harness join task panicked");
            AppError::Internal
        })?
        .map_err(|error| {
            tracing::error!(%run_id, error = %error, "harness run ended in error");
            AppError::Harness(error.to_string())
        })?;

    record_history(&state.db, run_id, thread_id, started_at, outcome).await
}

/// Publishes one event at the next `seq`, writing it first when it is durable.
///
/// Persist-then-publish is what keeps a subscriber from seeing an event a
/// reconnect would fail to replay. For a *folded* event that guarantee is
/// weaker by construction: a delta is published and never written, and the
/// message it belongs to only becomes durable when it closes. So a client
/// that reloads mid-message sees nothing of that message until `message_stop`
/// — the compacted event then arrives live and renders it whole. That window
/// is the price of not storing the deltas.
async fn publish(
    state: &AppState,
    sender: &broadcast::Sender<StreamEvent>,
    run_id: Uuid,
    seq: &mut i64,
    kind: String,
    payload: Value,
    persist: bool,
) -> ApiResult<()> {
    let at = *seq;
    *seq += 1;

    if persist {
        // Checked out per durable event rather than per streamed one: a
        // pool checkout for every delta was its own cost, next to the row.
        let mut conn = state.db.get().await?;
        diesel::insert_into(crate::schema::transcript::table)
            .values(&NewTranscript {
                id: Uuid::now_v7(),
                run_id,
                seq: at,
                kind: &kind,
                payload: payload.clone(),
                created_at: now_epoch(),
            })
            .execute(&mut conn)
            .await
            .map_err(|err| AppError::db(err, "run.insert_transcript"))?;
    }

    let _ = sender.send(StreamEvent {
        run_id,
        seq: at,
        kind,
        payload,
    });
    Ok(())
}

async fn mark_run_complete(db: &DbPool, run_id: Uuid) -> ApiResult<()> {
    let mut conn = db.get().await?;
    diesel::update(run::table.filter(run::id.eq(run_id)))
        .set(run::completed_at.eq(Some(now_epoch())))
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "run.mark_complete"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Persistence: the exchange log and the spine
// ---------------------------------------------------------------------------

/// One model frame, folded out of the frame log.
#[derive(Default)]
struct ModelFrame {
    request: Option<Vec<u8>>,
    events: Vec<llm::Event>,
    usage: Option<llm::Usage>,
    outcome: Option<Value>,
}

/// Writes what the frame log recorded, and advances the thread's spine.
///
/// One transaction: an exchange that exists without the spine position naming
/// it is recoverable garbage, but a spine position naming an exchange that was
/// never written is a broken lineage, and `next_seq` moving without either is
/// a hole in the chain.
async fn record_history(
    db: &DbPool,
    run_id: Uuid,
    thread_id: Uuid,
    started_at: i64,
    outcome: RunOutcome,
) -> ApiResult<()> {
    let frames = fold_model_frames(&outcome.frames);
    if frames.is_empty() {
        return Ok(());
    }

    let committed_lineage = match &outcome.committed_frame {
        Some(_) => Some(serde_json::to_vec(&outcome.committed).map_err(|error| {
            tracing::error!(%run_id, error = %error, "canonical lineage failed to encode");
            AppError::Internal
        })?),
        // No commit means no position to record: the harness streamed and
        // discarded, and the lineage did not move.
        None => None,
    };
    let committed_frame = outcome.committed_frame.clone();

    let mut conn = db.get().await?;
    conn.transaction::<_, AppError, _>(|conn| {
        async move {
            let now = now_epoch();
            let mut committed_exchange: Option<Uuid> = None;

            for (frame_id, frame) in &frames {
                let is_committed = committed_frame.as_deref() == Some(frame_id.as_str());

                let request_bytes = frame.request.clone().unwrap_or_default();
                let request_digest = put_blob(conn, &request_bytes, now).await?;

                let events_digest = match serde_json::to_vec(&frame.events) {
                    Ok(bytes) => Some(put_blob(conn, &bytes, now).await?),
                    Err(error) => {
                        tracing::error!(%run_id, error = %error, "provider events failed to encode");
                        None
                    }
                };

                let lineage_digest = match (is_committed, &committed_lineage) {
                    (true, Some(bytes)) => Some(put_blob(conn, bytes, now).await?),
                    _ => None,
                };

                let exchange_id = Uuid::now_v7();
                diesel::insert_into(exchange::table)
                    .values(&NewExchange {
                        id: exchange_id,
                        run_id,
                        request_blob_digest: &request_digest,
                        provider_events_digest: events_digest.as_deref(),
                        canonical_blob_digest: lineage_digest.as_deref(),
                        usage: frame.usage.map(usage_json),
                        outcome: frame.outcome.clone(),
                        // H6's cache accounting is not instrumented yet — the
                        // expectation is derivable from the request bytes
                        // retroactively, so recording zero costs history, not
                        // correctness.
                        expected_cache_tokens: 0,
                        started_at,
                        completed_at: Some(now),
                    })
                    .execute(conn)
                    .await
                    .map_err(|err| AppError::db(err, "run.insert_exchange"))?;

                if is_committed {
                    committed_exchange = Some(exchange_id);
                }
            }

            let Some(exchange_id) = committed_exchange else {
                return Ok(());
            };

            // The allocator is the thread's, and reading it under the same
            // transaction that writes the position is what keeps two
            // concurrent runs on one thread from claiming the same `seq`.
            let next_seq: i32 = diesel::update(thread::table.filter(thread::id.eq(thread_id)))
                .set(thread::next_seq.eq(thread::next_seq + 1))
                .returning(thread::next_seq)
                .get_result(conn)
                .await
                .map_err(|err| AppError::db(err, "run.advance_next_seq"))?;

            diesel::insert_into(spine::table)
                .values(&NewSpine {
                    thread_id,
                    seq: i64::from(next_seq - 1),
                    exchange_id,
                    // The harness called `commit` — under H4's eventual
                    // auto-commit this would be `false` for the default path,
                    // and `true` only for a deliberate best-of-N selection.
                    explicit_commit: true,
                    created_at: now,
                })
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "run.insert_spine"))?;

            Ok(())
        }
        .scope_boxed()
    })
    .await
}

/// Groups the frame log's model events by frame, in first-seen order.
///
/// Only `model` frames become exchanges: a `tool` or `harness` frame has no
/// request bytes and nothing the exchange table models.
fn fold_model_frames(events: &[CoreEvent]) -> Vec<(FrameId, ModelFrame)> {
    let mut order: Vec<FrameId> = Vec::new();
    let mut frames: HashMap<FrameId, ModelFrame> = HashMap::new();

    fn entry(
        frames: &mut HashMap<FrameId, ModelFrame>,
        order: &mut Vec<FrameId>,
        frame: &FrameId,
    ) {
        if !frames.contains_key(frame) {
            frames.insert(frame.clone(), ModelFrame::default());
            order.push(frame.clone());
        }
    }

    for event in events {
        match event {
            CoreEvent::FrameStart {
                frame,
                detail: FrameDetail::Model { .. },
                ..
            } => entry(&mut frames, &mut order, frame),
            CoreEvent::ModelRequest { frame, body } => {
                entry(&mut frames, &mut order, frame);
                if let Some(slot) = frames.get_mut(frame) {
                    slot.request = Some(body.clone());
                }
            }
            CoreEvent::ModelEvent { frame, event } => {
                if let Some(slot) = frames.get_mut(frame) {
                    slot.events.push(event.clone());
                }
            }
            CoreEvent::ModelUsage { frame, usage } => {
                if let Some(slot) = frames.get_mut(frame) {
                    slot.usage = Some(*usage);
                }
            }
            CoreEvent::FrameStop { frame, outcome } => {
                if let Some(slot) = frames.get_mut(frame) {
                    slot.outcome = Some(match outcome {
                        Outcome::Ok => json!({ "type": "ok" }),
                        Outcome::Failed { error } => json!({
                            "type": "failed",
                            "error": error,
                        }),
                    });
                }
            }
            _ => {}
        }
    }

    order
        .into_iter()
        .filter_map(|id| frames.remove(&id).map(|frame| (id, frame)))
        .collect()
}

fn usage_json(usage: llm::Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_input_tokens,
        "cache_write_tokens": usage.cache_creation_input_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
    })
}

/// Content-addressed insert. A digest already present is already the same
/// bytes, so a conflict is a hit, not a collision.
async fn put_blob(conn: &mut AsyncPgConnection, bytes: &[u8], now: i64) -> ApiResult<Vec<u8>> {
    let digest = Sha256::digest(bytes).to_vec();

    diesel::insert_into(blob::table)
        .values(&NewBlob {
            digest: &digest,
            data: Some(bytes),
            storage_path: None,
            byte_length: bytes.len() as i64,
            created_at: now,
        })
        .on_conflict(blob::digest)
        .do_nothing()
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "run.put_blob"))?;

    Ok(digest)
}

/// Writes the user's turn at [`INPUT_SEQ`] and returns it for publication.
pub async fn record_input(
    conn: &mut AsyncPgConnection,
    run_id: Uuid,
    input: &llm::Message,
) -> ApiResult<StreamEvent> {
    let payload = serde_json::to_value(harness::mapping::Message::from(input)).map_err(|error| {
        tracing::error!(error = %error, "input message failed to encode");
        AppError::Internal
    })?;

    diesel::insert_into(crate::schema::transcript::table)
        .values(&NewTranscript {
            id: Uuid::now_v7(),
            run_id,
            seq: INPUT_SEQ,
            kind: KIND_INPUT,
            payload: payload.clone(),
            created_at: now_epoch(),
        })
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "run.record_input"))?;

    Ok(StreamEvent {
        run_id,
        seq: INPUT_SEQ,
        kind: KIND_INPUT.to_owned(),
        payload,
    })
}
