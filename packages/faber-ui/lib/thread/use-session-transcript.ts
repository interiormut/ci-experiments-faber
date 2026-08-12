"use client"

import * as React from "react"

import {
  faber,
  FaberError,
  type StreamQuery,
  type TranscriptEvent,
  type Uuid,
} from "@/lib/api"
import {
  applyEvent,
  applyEvents,
  createStore,
  fromStreamEvent,
  fromTranscriptEvent,
  turnsOf,
  type Turn,
  type TranscriptStore,
} from "./transcript"

const TRANSCRIPT_LIMIT = 500
/** Backoff before retrying the live stream after a transient failure (e.g. a network blip). */
const RETRY_DELAY_MS = 1000

export type SessionTranscriptState = {
  turns: Turn[]
  /** True until the thread's durable history has loaded once. */
  loading: boolean
  error: string | null
  /** A run is in flight — drives the prompt box's send/interrupt affordance. */
  isRunning: boolean
}

async function loadHistory(threadId: Uuid): Promise<{ store: TranscriptStore; resume: StreamQuery }> {
  const runs = await faber.listRuns(threadId)
  let store = createStore()
  let resume: StreamQuery = {}

  for (const run of runs) {
    let events: TranscriptEvent[] = []
    let afterSeq: number | undefined
    for (;;) {
      const page = await faber.listTranscript(run.id, {
        after_seq: afterSeq,
        limit: TRANSCRIPT_LIMIT,
      })
      events = [...events, ...page]
      if (page.length < TRANSCRIPT_LIMIT) break
      afterSeq = page[page.length - 1]?.seq
      if (afterSeq === undefined) break
    }
    store = applyEvents(
      store,
      events.map((event) => fromTranscriptEvent(run.id, event)),
    )
    // Terminal SSE markers are intentionally live-only. On reload, the run
    // row's completion timestamp is the durable equivalent of `run_end`.
    if (run.completed_at !== null) {
      store = applyEvent(store, {
        runId: run.id,
        seq: -1,
        kind: "run_end",
        payload: null,
      })
    }
    if (run.completed_at === null) {
      const last = events[events.length - 1]
      resume = last ? { run_id: run.id, after_seq: last.seq } : {}
    }
  }

  return { store, resume }
}

/**
 * Everything happening in a session's root thread, live. Loads the durable
 * history first, then opens `streamSession` at the resume cursor so the
 * window between fetch and subscribe leaves no gap — a `lagged` disconnect
 * (or any other drop) refetches history and reopens rather than leaving a
 * hole in the transcript.
 *
 * Fork/multi-thread support is deliberately out of scope: this always follows
 * the session's root thread.
 *
 * Assumes `sessionId` is stable for the life of the calling component (`key`
 * it on `sessionId` at the call site) — `threadId` is expected to go from
 * `null` to set exactly once, so there is no mid-life reset to perform.
 */
export function useSessionTranscript(
  sessionId: Uuid | null,
  threadId: Uuid | null,
): SessionTranscriptState {
  const [store, setStore] = React.useState<TranscriptStore>(() => createStore())
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState<string | null>(null)

  React.useEffect(() => {
    if (!sessionId || !threadId) return

    let cancelled = false
    const controller = new AbortController()

    async function run() {
      const { store: history, resume } = await loadHistory(threadId!)
      if (cancelled) return
      setStore(history)
      setLoading(false)

      let cursor = resume
      while (!cancelled) {
        try {
          for await (const raw of faber.streamSession(sessionId!, cursor, controller.signal)) {
            if (cancelled) return
            setStore((prev) => applyEvent(prev, fromStreamEvent(raw)))
          }
          return // the generator returned — an intentional close (abort)
        } catch {
          if (cancelled || controller.signal.aborted) return
          await new Promise((resolve) => setTimeout(resolve, RETRY_DELAY_MS))
          if (cancelled) return
          const refreshed = await loadHistory(threadId!)
          if (cancelled) return
          setStore(refreshed.store)
          cursor = refreshed.resume
        }
      }
    }

    run().catch((err) => {
      if (cancelled) return
      setError(err instanceof FaberError ? err.message : "failed to load the thread")
      setLoading(false)
    })

    return () => {
      cancelled = true
      controller.abort()
    }
  }, [sessionId, threadId])

  const turns = React.useMemo(() => turnsOf(store), [store])
  const isRunning = turns.some((turn) => turn.status === "running")

  return { turns, loading, error, isRunning }
}
