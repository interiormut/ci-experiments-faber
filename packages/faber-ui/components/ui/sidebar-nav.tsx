"use client"

import type { ComponentType } from "react"
import { Plus } from "lucide-react"
import { motion } from "framer-motion"

import { cn } from "@/lib/utils"

type NavItem = {
  label: string
  icon: ComponentType<{ className?: string }>
  active?: boolean
}

type NavSection = {
  label?: string
  items: NavItem[]
}

export function SidebarNav({
  sections,
  newLabel,
  className,
}: {
  sections: NavSection[]
  newLabel?: string
  className?: string
}) {
  return (
    <nav
      className={cn(
        "flex h-full flex-col gap-2 rounded-xl border border-sidebar-border bg-sidebar p-3",
        className
      )}
    >
      {sections.map((section) => (
        <div key={section.label ?? section.items.map((item) => item.label).join("-")}>
          {section.label ? (
            <div className="px-2 py-2 text-[10.5px] font-medium uppercase tracking-[0.12em] text-muted-foreground/70">
              {section.label}
            </div>
          ) : null}
          <div className="space-y-0.5">
            {section.items.map((item) => {
              const Icon = item.icon

              return (
                <button
                  key={item.label}
                  type="button"
                  className={cn(
                    "relative flex w-full items-center gap-2.5 rounded-lg px-3 py-2.5 text-left text-[13.5px] transition-colors",
                    item.active
                      ? "bg-sidebar-accent font-medium text-foreground"
                      : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-foreground"
                  )}
                >
                  {item.active ? (
                    <motion.span
                      layoutId="sidebar-active"
                      className="absolute inset-y-1 left-0 w-0.5 rounded-full bg-primary"
                      transition={{ type: "spring", stiffness: 380, damping: 30 }}
                    />
                  ) : null}
                  <Icon className={cn("h-4 w-4 shrink-0", item.active && "text-primary")} />
                  <span>{item.label}</span>
                </button>
              )
            })}
          </div>
        </div>
      ))}

      {newLabel ? (
        <button
          type="button"
          className="mt-1 flex items-center gap-2 rounded-lg border border-dashed border-sidebar-border px-3 py-2 text-[13px] text-muted-foreground transition-colors hover:border-primary/60 hover:bg-primary/5 hover:text-primary"
        >
          <Plus className="h-4 w-4" />
          <span>{newLabel}</span>
        </button>
      ) : null}
    </nav>
  )
}
