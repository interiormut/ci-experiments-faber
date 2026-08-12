"use client"

import * as React from "react"
import { useParams } from "next/navigation"

import { faber, FaberError, type Uuid } from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import { PromptBox } from "@/components/thread/prompt-box"
import { ModelPicker } from "@/components/thread/model-picker"
import { TurnView } from "@/components/thread/turn"
import { useSessionTranscript } from "@/lib/thread/use-session-transcript"

/** How close to the bottom (px) still counts as "at the bottom" for autoscroll. */
const STICK_THRESHOLD = 96

export default function SessionPage() {
  const params = useParams<{ id: string }>()
  const sessionId = params.id as Uuid

  // Keyed on the session id so navigating between threads remounts this
  // subtree — a fresh set of `useState` defaults rather than an effect that
  // resets state mid-life.
  return <SessionThread key={sessionId} sessionId={sessionId} />
}

function SessionThread({ sessionId }: { sessionId: Uuid }) {
  const { models, modelsLoaded, selectedModel, selectModel } = useAppShell()

  // Fork/multi-thread support is out of scope — this page always follows the
  // session's root thread.
  const [threadId, setThreadId] = React.useState<Uuid | null>(null)
  const [threadError, setThreadError] = React.useState<string | null>(null)

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listThreads(sessionId)
      .then((threads) => {
        if (cancelled) return
        const root = threads.find((thread) => thread.parent_id === null) ?? threads[0] ?? null
        if (!root) {
          setThreadError("This thread has no root — it may have been deleted.")
          return
        }
        setThreadId(root.id)
      })
      .catch((err) => {
        if (cancelled) return
        setThreadError(err instanceof FaberError ? err.message : "failed to load the thread")
      })

    return () => {
      cancelled = true
    }
  }, [sessionId])

  const { turns, loading, error, isRunning } = useSessionTranscript(sessionId, threadId)

  const scrollRef = React.useRef<HTMLDivElement>(null)
  const stickToBottomRef = React.useRef(true)

  React.useEffect(() => {
    const el = scrollRef.current
    if (!el || !stickToBottomRef.current) return
    el.scrollTop = el.scrollHeight
  }, [turns])

  const handleScroll = () => {
    const el = scrollRef.current
    if (!el) return
    stickToBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < STICK_THRESHOLD
  }

  const [sendError, setSendError] = React.useState<string | null>(null)
  const noModels = modelsLoaded && models.length === 0

  const handleSend = React.useCallback(
    async (content: string) => {
      setSendError(null)

      const model = selectedModel?.alias
      if (!model || !threadId) {
        setSendError("Add a model before sending a message.")
        return false
      }

      try {
        await faber.sendMessage(sessionId, { content, model, thread_id: threadId })
        stickToBottomRef.current = true
        return true
      } catch (err) {
        setSendError(err instanceof FaberError ? err.message : "failed to send the message")
        return false
      }
    },
    [sessionId, threadId, selectedModel],
  )

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div ref={scrollRef} onScroll={handleScroll} className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-4xl flex-col gap-8 px-4 pt-8 pb-40">
          {threadError ? <p className="text-sm text-destructive">{threadError}</p> : null}
          {loading && !threadError ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : null}
          {turns.map((turn, index) => (
            <TurnView key={turn.runId} turn={turn} isLast={index === turns.length - 1} />
          ))}
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
        </div>
      </div>

      <div className="pointer-events-none absolute inset-x-0 bottom-0 flex flex-col items-center gap-2 px-4 pb-6">
        {sendError ? (
          <p className="pointer-events-none max-w-4xl text-center text-xs text-destructive">
            {sendError}
          </p>
        ) : null}
        <PromptBox
          className="pointer-events-auto w-full max-w-4xl"
          placeholder={noModels ? "Add a model to start chatting…" : "Message…"}
          sendDisabled={noModels || !threadId}
          isExecuting={isRunning}
          onSend={handleSend}
          footerActions={
            <ModelPicker
              models={models}
              selected={selectedModel}
              loaded={modelsLoaded}
              onSelect={selectModel}
            />
          }
        />
      </div>
    </div>
  )
}
