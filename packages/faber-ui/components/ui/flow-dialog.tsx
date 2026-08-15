"use client"

import * as React from "react"
import { AnimatePresence, motion } from "framer-motion"
import { ArrowLeft } from "lucide-react"

import {
  Dialog,
  DialogContent,
  DialogTitle,
} from "@/components/ui/responsive-dialog"
import { cn } from "@/lib/utils"

// ─── Constants ────────────────────────────────────────────────────────────

const EASE = [0.22, 1, 0.36, 1] as const
const TRANSITION_DURATION = 0.25

// ─── Height animation ─────────────────────────────────────────────────────

function useHeightMeasurement() {
  const ref = React.useRef<HTMLDivElement>(null)
  const [height, setHeight] = React.useState<number | null>(null)

  React.useEffect(() => {
    const el = ref.current
    if (!el) return

    const measure = () => {
      if (el.scrollHeight > 0) setHeight(el.scrollHeight)
    }

    const observer = new ResizeObserver(measure)
    observer.observe(el)
    measure()

    return () => observer.disconnect()
  }, [])

  return { ref, height }
}

/**
 * Animates its container's height to the measured content height.
 *
 * The `overflow: hidden` has to sit on the animated element itself — it is what
 * hides the excess while the box is mid-tween — so it clips at exactly the box
 * being animated. That makes the wrapper's width a contract: **the dialog's
 * padding belongs inside this element, not on an ancestor.** Fields paint a
 * `ring-4` focus ring outside their border box, and with the padding outside the
 * clip the box hugs the fields and shaves every ring. `display: flow-root` on
 * the measured child keeps that inner inset inside `scrollHeight`.
 */
function FlowDialogHeight({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  const { ref, height } = useHeightMeasurement()

  return (
    <motion.div
      className={className}
      animate={{ height: height ?? "auto" }}
      transition={{ duration: TRANSITION_DURATION, ease: EASE }}
      style={{ overflow: "hidden" }}
    >
      <div ref={ref} style={{ display: "flow-root" }}>
        {children}
      </div>
    </motion.div>
  )
}

// ─── Crossfade ────────────────────────────────────────────────────────────

/**
 * Swaps content keyed by `viewKey`: the outgoing view fades out, then — after a
 * matching pause — the incoming one fades in. `mode="wait"` keeps exactly one
 * view mounted, which is what lets {@link FlowDialogHeight} tween to a single
 * unambiguous natural height.
 *
 * It owns no height animation and adds no `overflow`, so it is safe to nest
 * inside a container that is already height-animated without clipping
 * edge-bleeding elements such as focus rings or negative margins.
 */
function FlowDialogCrossFade({
  viewKey,
  children,
  className,
}: {
  viewKey: string
  children: React.ReactNode
  className?: string
}) {
  return (
    <AnimatePresence mode="wait" initial={false}>
      <motion.div
        key={viewKey}
        className={className}
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{
          duration: TRANSITION_DURATION,
          ease: EASE,
          delay: TRANSITION_DURATION,
        }}
      >
        {children}
      </motion.div>
    </AnimatePresence>
  )
}

// ─── FlowDialog ───────────────────────────────────────────────────────────

interface FlowDialogProps {
  open: boolean
  onOpenChange?: (open: boolean) => void
  /**
   * Identifies the view currently in `children`. Changing it crossfades to the
   * new content and tweens the dialog's height to fit. Which view that is, and
   * how the user got there, is entirely the caller's business.
   */
  view: string
  /** Accessible name for the dialog. Rendered sr-only. */
  title: string
  /** Allow closing without finishing the flow (default: true). */
  dismissible?: boolean
  /** Classes for the dialog surface. */
  className?: string
  /** Classes for the padded box inside the height clip — override to retune the inset. */
  contentClassName?: string
  children: React.ReactNode
}

/**
 * A dialog whose body is one view at a time, crossfading between them while the
 * surface springs to each view's natural height. On viewports under 640px it
 * renders as a bottom drawer instead — see `responsive-dialog`.
 *
 * It holds no navigation state. Drive `view` from wherever the flow already
 * lives; a menu of `ActionRow`s that each `setView(...)` is the common case, but
 * the content is arbitrary — forms, confirmations, a wizard's steps.
 */
function FlowDialog({
  open,
  onOpenChange,
  view,
  title,
  dismissible = true,
  className,
  contentClassName,
  children,
}: FlowDialogProps) {
  const handleOpenChange = (next: boolean) => {
    if (!next && !dismissible) return
    onOpenChange?.(next)
  }

  const block = dismissible ? undefined : (e: Event | KeyboardEvent) => e.preventDefault()

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      {/*
        `p-0` hands the dialog's inset to the padded div below, inside the
        height animation's clip box — see `FlowDialogHeight`. The breakpoint
        here matches `useIsMobile`'s 640px, so the padding tracks which surface
        (dialog or drawer) is actually rendering.
      */}
      <DialogContent
        showCloseButton={dismissible}
        className={cn("p-0 sm:max-w-[440px]", className)}
        onEscapeKeyDown={block}
        onPointerDownOutside={block}
        onInteractOutside={block}
      >
        <DialogTitle className="sr-only">{title}</DialogTitle>
        <FlowDialogHeight>
          <div className={cn("px-6 pb-6 pt-4 sm:pt-6", contentClassName)}>
            <FlowDialogCrossFade viewKey={view}>{children}</FlowDialogCrossFade>
          </div>
        </FlowDialogHeight>
      </DialogContent>
    </Dialog>
  )
}

// ─── FlowDialogHeading ────────────────────────────────────────────────────

interface FlowDialogHeadingProps {
  title: React.ReactNode
  description?: React.ReactNode
  /** Brand mark or icon rendered above the title. */
  mark?: React.ReactNode
  className?: string
}

/** The title block a view opens with. Optional — a view can render anything. */
function FlowDialogHeading({
  title,
  description,
  mark,
  className,
}: FlowDialogHeadingProps) {
  return (
    <div
      data-slot="flow-dialog-heading"
      className={cn("mb-[22px] flex flex-col gap-1.5", className)}
    >
      {mark && <div className="mb-2.5 flex">{mark}</div>}
      <h2 className="text-lg font-semibold tracking-[-0.01em] text-foreground">
        {title}
      </h2>
      {description && (
        <p className="text-[13px] leading-relaxed text-muted-foreground">
          {description}
        </p>
      )}
    </div>
  )
}

// ─── FlowDialogBack ───────────────────────────────────────────────────────

/**
 * The footer affordance that returns to the previous view. It is a plain
 * button — the caller decides what "back" means by setting `view`.
 */
function FlowDialogBack({
  children = "Back",
  className,
  type = "button",
  ...props
}: React.ComponentProps<"button">) {
  return (
    <div className="mt-[22px] flex flex-col items-center gap-3.5">
      <button
        type={type}
        data-slot="flow-dialog-back"
        className={cn(
          "inline-flex items-center gap-1 text-[13px] font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:text-foreground focus-visible:outline-none",
          className,
        )}
        {...props}
      >
        <ArrowLeft size={14} />
        {children}
      </button>
    </div>
  )
}

// ─── Exports ──────────────────────────────────────────────────────────────

export {
  FlowDialog,
  FlowDialogBack,
  FlowDialogCrossFade,
  FlowDialogHeading,
  FlowDialogHeight,
}
export type { FlowDialogProps, FlowDialogHeadingProps }
