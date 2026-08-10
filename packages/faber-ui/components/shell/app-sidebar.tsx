"use client"

import {
  MessageSquare,
  MessageSquareDashed,
  MessageSquareText,
  Plus,
} from "lucide-react"

import { FaberLogo } from "@/components/ui/logos"
import { Button } from "@/components/ui/button"
import { SidebarNav } from "@/components/ui/sidebar-nav"
import { ProfileMenu } from "@/components/shell/profile-menu"

export function AppSidebar() {
  return (
    <aside className="flex w-64 shrink-0 flex-col border-r border-border bg-sidebar/60 text-sidebar-foreground">
      <div className="flex items-center gap-3 px-5 pt-5 pb-4">
        <FaberLogo size={28} aria-hidden />
        <span className="text-[15px] font-semibold tracking-tight">Faber</span>
      </div>

      <div className="px-3 pb-3">
        <Button size="lg" className="w-full justify-start gap-2">
          <Plus className="h-4 w-4" />
          New thread
        </Button>
      </div>

      <SidebarNav
        ariaLabel="Sidebar navigation"
        className="flex-1 overflow-y-auto rounded-none border-none bg-transparent p-0 px-2 py-1"
        sections={[
          {
            label: "Threads",
            items: [
              {
                label: "Harness design review",
                icon: MessageSquareText,
                active: true,
              },
              { label: "Refactor the build", icon: MessageSquare },
              { label: "Open questions", icon: MessageSquareDashed },
            ],
          },
        ]}
      />

      <div className="border-t border-sidebar-border p-2">
        <ProfileMenu />
      </div>
    </aside>
  )
}
