"use client"

import * as React from "react"

import { faber, FaberError, type ModelConfig, type Session, type Uuid } from "@/lib/api"
import { AppSidebar } from "@/components/shell/app-sidebar"
import { PromptBox } from "@/components/thread/prompt-box"

export default function Home() {
  const [sessions, setSessions] = React.useState<Session[]>([])
  const [sessionsLoading, setSessionsLoading] = React.useState(true)
  const [activeSessionId, setActiveSessionId] = React.useState<Uuid | null>(null)
  const [creatingSession, setCreatingSession] = React.useState(false)

  const [models, setModels] = React.useState<ModelConfig[]>([])
  const [modelsLoaded, setModelsLoaded] = React.useState(false)

  const [sendError, setSendError] = React.useState<string | null>(null)

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listSessions()
      .then((rows) => {
        if (cancelled) return
        setSessions(rows)
        setActiveSessionId((current) => current ?? rows[0]?.id ?? null)
      })
      .catch(() => {
        // Left empty: the sidebar's empty state is indistinguishable from a
        // failed fetch, which is fine for a first pass — a retry happens on
        // the next visit.
      })
      .finally(() => {
        if (!cancelled) setSessionsLoading(false)
      })

    void faber
      .listModels()
      .then((rows) => {
        if (!cancelled) setModels(rows)
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setModelsLoaded(true)
      })

    return () => {
      cancelled = true
    }
  }, [])

  const createSession = React.useCallback(async (): Promise<Session | null> => {
    setCreatingSession(true)
    try {
      const created = await faber.createSession()
      setSessions((prev) => [created, ...prev])
      setActiveSessionId(created.id)
      return created
    } catch (err) {
      setSendError(err instanceof FaberError ? err.message : "failed to create a thread")
      return null
    } finally {
      setCreatingSession(false)
    }
  }, [])

  const handleSend = React.useCallback(
    async (content: string) => {
      setSendError(null)

      const model = models[0]?.alias
      if (!model) {
        setSendError("Add a model before starting a thread.")
        return false
      }

      let sessionId = activeSessionId
      if (!sessionId) {
        const created = await createSession()
        if (!created) return false
        sessionId = created.id
      }

      try {
        await faber.sendMessage(sessionId, { content, model })
        return true
      } catch (err) {
        setSendError(err instanceof FaberError ? err.message : "failed to send the message")
        return false
      }
    },
    [activeSessionId, createSession, models],
  )

  const noModels = modelsLoaded && models.length === 0

  return (
    <div className="relative flex min-h-0 flex-1">
      <AppSidebar
        sessions={sessions}
        activeSessionId={activeSessionId}
        onSelectSession={setActiveSessionId}
        onCreateSession={() => void createSession()}
        loading={sessionsLoading}
        creating={creatingSession}
      />
      <main className="flex min-w-0 flex-1 flex-col items-center justify-center gap-3 p-4">
        <PromptBox
          className="w-full max-w-2xl"
          placeholder={noModels ? "Add a model to start chatting…" : "Start a thread…"}
          sendDisabled={noModels}
          onSend={handleSend}
        />
        {sendError ? (
          <p className="w-full max-w-2xl text-sm text-destructive">{sendError}</p>
        ) : null}
      </main>
    </div>
  )
}
