"use client"

import { Cpu, KeyRound, MessageSquare, MessageSquareText, Plus } from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { FaberLogo } from "@/components/ui/logos"
import { Button } from "@/components/ui/button"
import { SidebarNav } from "@/components/ui/sidebar-nav"
import { ProfileMenu } from "@/components/shell/profile-menu"

/** The sidebar's one active-item key, shared across every kind of nav row it renders. */
export function sessionNavKey(id: Uuid): string {
  return `session:${id}`
}

export type AppSidebarProps = {
  sessions: Session[]
  /** Which row is current — `sessionNavKey(id)`, `"models"`, `"credentials"`, or `null`. */
  activeNavKey: string | null
  onSelectSession: (id: Uuid) => void
  onSelectModels: () => void
  onSelectCredentials: () => void
  onCreateSession: () => void
  loading?: boolean
  creating?: boolean
}

function sessionLabel(session: Session): string {
  return session.title?.trim() || "Untitled thread"
}

export function AppSidebar({
  sessions,
  activeNavKey,
  onSelectSession,
  onSelectModels,
  onSelectCredentials,
  onCreateSession,
  loading = false,
  creating = false,
}: AppSidebarProps) {
  return (
    <aside className="flex h-full w-64 shrink-0 flex-col overflow-hidden border-r border-border bg-sidebar/60 text-sidebar-foreground">
      <div className="flex items-center gap-3 px-5 pt-5 pb-4">
        <FaberLogo size={28} aria-hidden />
        <span className="text-[15px] font-semibold tracking-tight">Faber</span>
      </div>

      <div className="px-3 pb-3">
        <Button
          size="lg"
          className="w-full justify-start gap-2"
          onClick={onCreateSession}
          loading={creating}
          loadingText="New thread"
        >
          <Plus className="h-4 w-4" />
          New thread
        </Button>
      </div>

      <div className="px-2 pb-2">
        <SidebarNav
          ariaLabel="Primary navigation"
          className="rounded-none border-none bg-transparent p-0"
          sections={[
            {
              items: [
                {
                  label: "Models",
                  icon: Cpu,
                  active: activeNavKey === "models",
                  onClick: onSelectModels,
                },
                {
                  label: "Credentials",
                  icon: KeyRound,
                  active: activeNavKey === "credentials",
                  onClick: onSelectCredentials,
                },
              ],
            },
          ]}
        />
      </div>

      {!loading && sessions.length === 0 ? (
        <div className="min-h-0 flex-1 overflow-y-auto px-5 pt-2">
          <p className="text-[13px] text-muted-foreground">No threads yet.</p>
        </div>
      ) : (
        <SidebarNav
          ariaLabel="Sidebar navigation"
          className="min-h-0 flex-1 overflow-y-auto rounded-none border-none bg-transparent p-0 px-2 py-1"
          sections={[
            {
              label: "Threads",
              items: loading
                ? []
                : sessions.map((session) => {
                    const key = sessionNavKey(session.id)
                    return {
                      label: sessionLabel(session),
                      icon: key === activeNavKey ? MessageSquareText : MessageSquare,
                      active: key === activeNavKey,
                      onClick: () => onSelectSession(session.id),
                    }
                  }),
            },
          ]}
        />
      )}

      <div className="border-t border-sidebar-border p-2">
        <ProfileMenu />
      </div>
    </aside>
  )
}
