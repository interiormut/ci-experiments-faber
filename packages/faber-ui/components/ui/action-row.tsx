import * as React from "react"
import { ChevronRight } from "lucide-react"

import { cn } from "@/lib/utils"

// ─── ActionRowGroup ───────────────────────────────────────────────────────

/**
 * The card frame a run of {@link ActionRow}s sits in: rounded, bordered, with
 * hairline dividers between rows. Rows are clipped to the rounded corners, so
 * a row's hover fill follows the frame at the top and bottom of the list.
 */
function ActionRowGroup({ className, children, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="action-row-group"
      className={cn(
        "divide-y divide-border/50 overflow-hidden rounded-2xl border border-border/60 bg-card",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  )
}

// ─── ActionRow ────────────────────────────────────────────────────────────

interface ActionRowProps extends Omit<React.ComponentProps<"button">, "onSelect"> {
  /** Leading glyph, rendered in a tinted square. Sized to 20px by the wrapper. */
  icon?: React.ReactNode
  label: React.ReactNode
  /** Secondary line under the label. Truncates to one line. */
  description?: React.ReactNode
  /** Show a trailing chevron — the convention for "opens another level". */
  chevron?: boolean
  /** Paint the row in the destructive palette. */
  destructive?: boolean
  /** Extra classes for the icon square (e.g. a per-item accent tint). */
  iconClassName?: string
  onSelect?: () => void
}

/**
 * One row of a settings/navigation list: icon, label, optional description,
 * optional trailing chevron.
 *
 * It is a plain `<button>`, deliberately not a Radix menu item — it carries no
 * roving focus or type-ahead, so it is equally at home in a drawer, a dialog,
 * or a static settings page. Wrap a run of them in {@link ActionRowGroup}.
 */
function ActionRow({
  icon,
  label,
  description,
  chevron,
  destructive = false,
  iconClassName,
  onSelect,
  className,
  type = "button",
  ...props
}: ActionRowProps) {
  return (
    <button
      type={type}
      data-slot="action-row"
      data-destructive={destructive || undefined}
      onClick={onSelect}
      className={cn(
        "flex w-full items-center gap-3 px-4 py-3 text-left text-sm transition-colors",
        "hover:bg-accent/50 focus-visible:bg-accent/50 focus-visible:outline-none",
        "disabled:pointer-events-none disabled:opacity-50",
        destructive && "text-destructive hover:bg-destructive/10",
        className,
      )}
      {...props}
    >
      {icon && (
        <span
          className={cn(
            "flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted [&_svg]:size-5 [&_svg]:shrink-0",
            destructive && "bg-destructive/10",
            iconClassName,
          )}
        >
          {icon}
        </span>
      )}
      <div className="min-w-0 flex-1">
        <div className="truncate font-medium">{label}</div>
        {description && (
          <div className="truncate text-xs text-muted-foreground">{description}</div>
        )}
      </div>
      {chevron && (
        <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
      )}
    </button>
  )
}

// ─── Exports ──────────────────────────────────────────────────────────────

export { ActionRow, ActionRowGroup }
export type { ActionRowProps }
