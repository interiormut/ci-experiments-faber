/**
 * Turns the wire vocabulary a run yields (`crates/harness/src/mapping.rs`'s
 * `LLMEvent` + the API's `tool_result`/`input`/`run_end`/`run_error`
 * additions, `crates/api/src/run.rs`) into what {@link AgentRun} renders.
 *
 * Fed from two sources that share the same `{kind, payload}` shape —
 * `listTranscript` (durable, per run) and `streamSession` (live, per session)
 * — through the same `applyEvent`, so replay and live share one code path.
 */

import type { AgentRunItem, AgentStepState } from "@/components/ui/agent-run"
import type { JsonValue, StreamEvent, TranscriptEvent, Uuid } from "@/lib/api"

// ---------------------------------------------------------------------------
// Wire shapes this module actually reads. Only the fields consumed here are
// modeled — everything else in a payload is ignored, deliberately: `kind` is
// free-form on the wire (history-abstract.md H8.7) and new variants must not
// require a type change to keep compiling.
// ---------------------------------------------------------------------------

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; signature?: string }
  | { type: "tool_use"; id: string; name: string; input: JsonValue }
  | { type: "tool_result"; toolUseId: string; content: string; isError?: boolean }
  | { type: "unknown"; raw: JsonValue }

export type TranscriptMessage = {
  id?: string
  role: "system" | "user" | "assistant"
  content: ContentBlock[]
}

type BlockStartPayload =
  | { type: "text" }
  | { type: "thinking" }
  | { type: "tool_use"; id: string; name: string }
  | { type: "unknown"; raw: JsonValue }

type DeltaPayload =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "thinking_signature"; signature: string }
  | { type: "tool_input_json"; partialJson: string }
  | { type: "unknown"; raw: JsonValue }

/** A normalized event, uniform across the replay and live sources. */
export type NormalizedEvent = {
  runId: Uuid
  seq: number
  kind: string
  payload: JsonValue
}

export function fromTranscriptEvent(runId: Uuid, event: TranscriptEvent): NormalizedEvent {
  return { runId, seq: event.seq, kind: event.kind, payload: event.payload }
}

export function fromStreamEvent(event: StreamEvent): NormalizedEvent {
  return { runId: event.run_id, seq: event.seq, kind: event.kind, payload: event.payload }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

export type RunStatus = "running" | "done" | "error"

export type Turn = {
  runId: Uuid
  /** The user's own turn (`kind: "input"`, seq 0) — not an `AgentRun` row. */
  userContent: ContentBlock[]
  items: AgentRunItem[]
  status: RunStatus
  errorMessage?: string
}

type BlockEntry =
  | { kind: "message"; itemId: string }
  | { kind: "tool"; itemId: string; partialJson: string }

export type TranscriptStore = {
  order: Uuid[]
  byRun: Record<Uuid, Turn>
  /** Open content blocks for the run's *current* LLM message, by block index. */
  blocks: Record<Uuid, Record<number, BlockEntry>>
  /** Seqs already applied per run — replay and live overlap at the resume cursor. */
  seen: Record<Uuid, Record<number, true>>
}

export function createStore(): TranscriptStore {
  return { order: [], byRun: {}, blocks: {}, seen: {} }
}

export function turnsOf(store: TranscriptStore): Turn[] {
  return store.order.map((id) => store.byRun[id])
}

function ensureRun(store: TranscriptStore, runId: Uuid): TranscriptStore {
  if (store.byRun[runId]) return store
  return {
    order: [...store.order, runId],
    byRun: {
      ...store.byRun,
      [runId]: { runId, userContent: [], items: [], status: "running" },
    },
    blocks: { ...store.blocks, [runId]: {} },
    seen: { ...store.seen, [runId]: {} },
  }
}

function updateRun(store: TranscriptStore, runId: Uuid, patch: Partial<Turn>): TranscriptStore {
  const current = store.byRun[runId]
  return { ...store, byRun: { ...store.byRun, [runId]: { ...current, ...patch } } }
}

function updateItem(
  store: TranscriptStore,
  runId: Uuid,
  itemId: string,
  update: (item: AgentRunItem) => AgentRunItem,
): TranscriptStore {
  const run = store.byRun[runId]
  const items = run.items.map((item) => (item.id === itemId ? update(item) : item))
  return updateRun(store, runId, { items })
}

function setBlock(
  store: TranscriptStore,
  runId: Uuid,
  index: number,
  entry: BlockEntry | undefined,
): TranscriptStore {
  const runBlocks = { ...store.blocks[runId] }
  if (entry) runBlocks[index] = entry
  else delete runBlocks[index]
  return { ...store, blocks: { ...store.blocks, [runId]: runBlocks } }
}

/** Applies one event, in `seq` order, to a store — pure, returns a new store. */
export function applyEvent(store: TranscriptStore, event: NormalizedEvent): TranscriptStore {
  const { runId, seq, kind, payload } = event

  if (store.seen[runId]?.[seq]) return store // replay/live overlap at the resume cursor

  let next = ensureRun(store, runId)
  next = { ...next, seen: { ...next.seen, [runId]: { ...next.seen[runId], [seq]: true } } }

  switch (kind) {
    case "input": {
      const message = payload as unknown as TranscriptMessage
      return updateRun(next, runId, { userContent: message.content ?? [] })
    }

    case "message_start": {
      // A fresh LLM message starts a fresh set of block indices.
      return { ...next, blocks: { ...next.blocks, [runId]: {} } }
    }

    case "block_start": {
      const { index, block } = payload as unknown as { index: number; block: BlockStartPayload }
      if (block.type === "text") {
        const itemId = `${runId}:${seq}`
        const run = next.byRun[runId]
        const items: AgentRunItem[] = [...run.items, { kind: "message", id: itemId, text: "" }]
        next = updateRun(next, runId, { items })
        return setBlock(next, runId, index, { kind: "message", itemId })
      }
      if (block.type === "tool_use") {
        const run = next.byRun[runId]
        const items: AgentRunItem[] = [
          ...run.items,
          { kind: "tool", id: block.id, name: block.name, state: "pending" },
        ]
        next = updateRun(next, runId, { items })
        return setBlock(next, runId, index, { kind: "tool", itemId: block.id, partialJson: "" })
      }
      // Thinking and unknown blocks are deliberately not surfaced on the
      // timeline — leave the index unmapped, so their deltas are dropped too.
      return next
    }

    case "block_delta": {
      const { index, delta } = payload as unknown as { index: number; delta: DeltaPayload }
      const entry = next.blocks[runId]?.[index]
      if (!entry) return next // an unmapped (thinking/unknown) block
      if (entry.kind === "message" && delta.type === "text") {
        return updateItem(next, runId, entry.itemId, (item) =>
          item.kind === "message" ? { ...item, text: item.text + delta.text } : item,
        )
      }
      if (entry.kind === "tool" && delta.type === "tool_input_json") {
        const partialJson = entry.partialJson + delta.partialJson
        return setBlock(next, runId, index, { ...entry, partialJson })
      }
      return next
    }

    case "block_stop": {
      const { index } = payload as unknown as { index: number }
      const entry = next.blocks[runId]?.[index]
      if (!entry || entry.kind !== "tool") return next
      const input = parseJsonLoosely(entry.partialJson)
      return updateItem(next, runId, entry.itemId, (item) =>
        item.kind === "tool" ? { ...item, state: "running" as AgentStepState, input } : item,
      )
    }

    case "tool_result": {
      const { toolUseId, content, isError } = payload as unknown as {
        toolUseId: string
        content: string
        isError?: boolean
      }
      return updateItem(next, runId, toolUseId, (item) =>
        item.kind === "tool"
          ? { ...item, state: isError ? "error" : "success", result: content }
          : item,
      )
    }

    case "run_end":
      return updateRun(next, runId, { status: "done" })

    case "run_error": {
      const message = (payload as { message?: string } | null)?.message
      return updateRun(next, runId, { status: "error", errorMessage: message })
    }

    default:
      // message_delta / message_stop and anything future-added carry nothing
      // this timeline renders.
      return next
  }
}

export function applyEvents(store: TranscriptStore, events: NormalizedEvent[]): TranscriptStore {
  return events.reduce(applyEvent, store)
}

function parseJsonLoosely(raw: string): JsonValue {
  if (!raw.trim()) return {}
  try {
    return JSON.parse(raw) as JsonValue
  } catch {
    return { raw }
  }
}
