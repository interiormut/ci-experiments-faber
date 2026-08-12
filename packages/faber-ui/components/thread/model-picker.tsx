"use client"

import * as React from "react"
import { Check, ChevronsUpDown } from "lucide-react"

import { cn } from "@/lib/utils"
import type { ModelConfig } from "@/lib/api"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

export type ModelPickerProps = {
  models: ModelConfig[]
  /** The model in effect — `null` while the list is still empty. */
  selected: ModelConfig | null
  onSelect: (alias: string) => void
  /** False while the model list is still in flight, so "none" isn't claimed early. */
  loaded?: boolean
  disabled?: boolean
  className?: string
}

/** Which model the next message goes to, chosen from the prompt box footer. */
export function ModelPicker({
  models,
  selected,
  onSelect,
  loaded = true,
  disabled = false,
  className,
}: ModelPickerProps) {
  const empty = models.length === 0

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          disabled={disabled || empty}
          aria-label="Select model"
          className={cn(
            "flex min-w-0 items-center gap-1 rounded-lg px-2 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50",
            className,
          )}
        >
          <span className="truncate">
            {selected?.alias ?? (loaded ? "No model" : "…")}
          </span>
          <ChevronsUpDown className="size-3.5 shrink-0 opacity-60" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="min-w-48">
        {models.map((model) => (
          <DropdownMenuItem
            key={model.id}
            onSelect={() => onSelect(model.alias)}
            className="flex items-center justify-between gap-3"
          >
            <span className="min-w-0">
              <span className="block truncate">{model.alias}</span>
              <span className="block truncate text-xs text-muted-foreground">
                {model.wire_id}
              </span>
            </span>
            {model.id === selected?.id ? <Check className="size-4 shrink-0" /> : null}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
