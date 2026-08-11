"use client"

import * as React from "react"
import { usePathname, useRouter } from "next/navigation"

import {
  faber,
  FaberError,
  type CreateModelRequest,
  type CreatedSession,
  type ModelConfig,
  type Session,
  type UpdateModelRequest,
  type Uuid,
} from "@/lib/api"
import { AppSidebar, sessionNavKey } from "@/components/shell/app-sidebar"

type AppShellContextValue = {
  sessions: Session[]
  sessionsLoading: boolean
  models: ModelConfig[]
  modelsLoaded: boolean
  creatingSession: boolean
  createError: string | null
  createSession: () => Promise<CreatedSession | null>
  renameSession: (id: Uuid, title: string) => Promise<Session>
  deleteSession: (id: Uuid) => Promise<void>
  addModel: (body: CreateModelRequest) => Promise<ModelConfig>
  editModel: (id: Uuid, patch: UpdateModelRequest) => Promise<ModelConfig>
  removeModel: (id: Uuid) => Promise<void>
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
  const activeNavKey: string | null = pathname?.startsWith(SESSION_PATH_PREFIX)
    ? sessionNavKey(pathname.slice(SESSION_PATH_PREFIX.length))
    : pathname === "/models"
      ? "models"
      : pathname === "/credentials"
        ? "credentials"
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

  const renameSession = React.useCallback(async (id: Uuid, title: string): Promise<Session> => {
    const updated = await faber.updateSession(id, { title: title.trim() || null })
    setSessions((prev) => prev.map((session) => (session.id === id ? updated : session)))
    return updated
  }, [])

  const deleteSession = React.useCallback(
    async (id: Uuid): Promise<void> => {
      await faber.deleteSession(id)
      setSessions((prev) => prev.filter((session) => session.id !== id))
      if (activeNavKey === sessionNavKey(id)) router.push("/")
    },
    [activeNavKey, router],
  )

  const addModel = React.useCallback(async (body: CreateModelRequest): Promise<ModelConfig> => {
    const created = await faber.createModel(body)
    setModels((prev) => [...prev, created])
    return created
  }, [])

  const editModel = React.useCallback(
    async (id: Uuid, patch: UpdateModelRequest): Promise<ModelConfig> => {
      const updated = await faber.updateModel(id, patch)
      setModels((prev) => prev.map((model) => (model.id === id ? updated : model)))
      return updated
    },
    [],
  )

  const removeModel = React.useCallback(async (id: Uuid): Promise<void> => {
    await faber.deleteModel(id)
    setModels((prev) => prev.filter((model) => model.id !== id))
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
      renameSession,
      deleteSession,
      addModel,
      editModel,
      removeModel,
    }),
    [
      sessions,
      sessionsLoading,
      models,
      modelsLoaded,
      creatingSession,
      createError,
      createSession,
      renameSession,
      deleteSession,
      addModel,
      editModel,
      removeModel,
    ],
  )

  return (
    <AppShellContext.Provider value={value}>
      <div className="relative flex min-h-0 flex-1">
        <AppSidebar
          sessions={sessions}
          activeNavKey={activeNavKey}
          onSelectSession={(id) => router.push(`/session/${id}`)}
          onSelectModels={() => router.push("/models")}
          onSelectCredentials={() => router.push("/credentials")}
          onCreateSession={() => {
            void createSession().then((created) => {
              if (created) router.push(`/session/${created.id}`)
            })
          }}
          onRenameSession={renameSession}
          onDeleteSession={deleteSession}
          loading={sessionsLoading}
          creating={creatingSession}
        />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">{children}</main>
      </div>
    </AppShellContext.Provider>
  )
}
