"use client"

import * as React from "react"
import { usePathname, useRouter } from "next/navigation"

import { faber, FaberError, type CreatedSession, type ModelConfig, type Session, type Uuid } from "@/lib/api"
import { AppSidebar } from "@/components/shell/app-sidebar"

type AppShellContextValue = {
  sessions: Session[]
  sessionsLoading: boolean
  models: ModelConfig[]
  modelsLoaded: boolean
  creatingSession: boolean
  createError: string | null
  createSession: () => Promise<CreatedSession | null>
}

const AppShellContext = React.createContext<AppShellContextValue | null>(null)

/** Sessions, models, and thread creation — shared by the landing page and every session page. */
export function useAppShell(): AppShellContextValue {
  const ctx = React.useContext(AppShellContext)
  if (!ctx) throw new Error("useAppShell must be used within <AppShell>")
  return ctx
}

const SESSION_PATH_PREFIX = "/session/"

/**
 * The app frame: sidebar plus the session/model state it and every page under
 * it need. Lives in the root layout so it persists across navigations between
 * threads — the sidebar's list and scroll position survive a route change.
 */
export function AppShell({ children }: { children: React.ReactNode }) {
  const router = useRouter()
  const pathname = usePathname()
  const activeSessionId: Uuid | null = pathname?.startsWith(SESSION_PATH_PREFIX)
    ? pathname.slice(SESSION_PATH_PREFIX.length)
    : null

  const [sessions, setSessions] = React.useState<Session[]>([])
  const [sessionsLoading, setSessionsLoading] = React.useState(true)
  const [models, setModels] = React.useState<ModelConfig[]>([])
  const [modelsLoaded, setModelsLoaded] = React.useState(false)
  const [creatingSession, setCreatingSession] = React.useState(false)
  const [createError, setCreateError] = React.useState<string | null>(null)

  React.useEffect(() => {
    let cancelled = false

    void faber
      .listSessions()
      .then((rows) => {
        if (!cancelled) setSessions(rows)
      })
      .catch(() => {
        // Empty sidebar is indistinguishable from a failed fetch, which is
        // fine for a first pass — a retry happens on the next visit.
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

  const createSession = React.useCallback(async (): Promise<CreatedSession | null> => {
    setCreatingSession(true)
    setCreateError(null)
    try {
      const created = await faber.createSession()
      setSessions((prev) => [created, ...prev])
      return created
    } catch (err) {
      setCreateError(err instanceof FaberError ? err.message : "failed to create a thread")
      return null
    } finally {
      setCreatingSession(false)
    }
  }, [])

  const value = React.useMemo<AppShellContextValue>(
    () => ({
      sessions,
      sessionsLoading,
      models,
      modelsLoaded,
      creatingSession,
      createError,
      createSession,
    }),
    [sessions, sessionsLoading, models, modelsLoaded, creatingSession, createError, createSession],
  )

  return (
    <AppShellContext.Provider value={value}>
      <div className="relative flex min-h-0 flex-1">
        <AppSidebar
          sessions={sessions}
          activeSessionId={activeSessionId}
          onSelectSession={(id) => router.push(`/session/${id}`)}
          onCreateSession={() => {
            void createSession().then((created) => {
              if (created) router.push(`/session/${created.id}`)
            })
          }}
          loading={sessionsLoading}
          creating={creatingSession}
        />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">{children}</main>
      </div>
    </AppShellContext.Provider>
  )
}
