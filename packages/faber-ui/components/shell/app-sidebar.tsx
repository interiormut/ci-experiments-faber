"use client"

import { MessageSquare, MessageSquareText, Plus } from "lucide-react"

import type { Session, Uuid } from "@/lib/api"
import { FaberLogo } from "@/components/ui/logos"
import { Button } from "@/components/ui/button"
import { SidebarNav } from "@/components/ui/sidebar-nav"
import { ProfileMenu } from "@/components/shell/profile-menu"

export type AppSidebarProps = {
  sessions: Session[]
  activeSessionId: Uuid | null
  onSelectSession: (id: Uuid) => void
  onCreateSession: () => void
  loading?: boolean
  creating?: boolean
}

function sessionLabel(session: Session): string {
  return session.title?.trim() || "Untitled thread"
}

export function AppSidebar({
  sessions,
  activeSessionId,
  onSelectSession,
  onCreateSession,
  loading = false,
  creating = false,
}: AppSidebarProps) {
  return (
    <aside className="flex w-64 shrink-0 flex-col border-r border-border bg-sidebar/60 text-sidebar-foreground">
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

      {!loading && sessions.length === 0 ? (
        <div className="flex-1 overflow-y-auto px-5 pt-2">
          <p className="text-[13px] text-muted-foreground">No threads yet.</p>
        </div>
      ) : (
        <SidebarNav
          ariaLabel="Sidebar navigation"
          className="flex-1 overflow-y-auto rounded-none border-none bg-transparent p-0 px-2 py-1"
          sections={[
            {
              label: "Threads",
              items: loading
                ? []
                : sessions.map((session) => ({
                    label: sessionLabel(session),
                    icon: session.id === activeSessionId ? MessageSquareText : MessageSquare,
                    active: session.id === activeSessionId,
                    onClick: () => onSelectSession(session.id),
                  })),
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
