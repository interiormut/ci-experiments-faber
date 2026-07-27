"use client"

import * as React from "react"
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
  useTransform,
  type MotionValue,
} from "framer-motion"
import { Check, X } from "lucide-react"

import { PANIT_DEFAULT_EASE } from "@/lib/motion"
import { cn } from "@/lib/utils"

/**
 * Agent conversation timeline.
 *
 * A single vertical line runs down the left edge through the whole turn. Tool
 * calls ({@link AgentStep}) bulge the line into a large circular node whose
 * inner disc carries the call's state. Agent prose ({@link AgentMessage}) rides
 * the same line with no node.
 *
 * The reveal is the point of the component. Each row owns exactly one
 * `progress` MotionValue. The line's draw and the ring's stroke are both
 * `useTransform`s of that value — no second animation, no matched keyframes —
 * so the circle does not have its own motion: it is the line's stroke
 * continuing around the ring. Because the driver is per-row and not a global
 * height map, streaming content of any length works.
 *
 * Rows do not animate themselves. {@link AgentRun} owns a coordinator
 * that batches every row mounted in the same commit into ONE animation: each
 * row becomes a linear slice of a single eased master value, so the ease
 * spans the whole chain and the tip crosses row boundaries at full speed —
 * one continuous draw, not N reveals in a row (or worse, in parallel). Rows
 * that stream in later form their own batch, queued behind whatever is still
 * drawing.
 */

export type AgentStepState = "pending" | "running" | "success" | "error"

type RunContextValue = {
  nodeSize: number
  lineWidth: number
  reduce: boolean
  /** Registers a row's progress value with the reveal coordinator. */
  enroll: (progress: MotionValue<number>, duration: number) => () => void
}

const RunContext = React.createContext<RunContextValue | null>(null)

function useRun() {
  const ctx = React.useContext(RunContext)
  if (!ctx) {
    throw new Error("AgentStep and AgentMessage must be used within <AgentRun>")
  }
  return ctx
}

// Fractions of the per-row progress spent on each phase of the reveal. One
// stroke is traced by the single progress value: the line draws down to the top
// of the node (lead), then flows around both sides of the ring and meets at the
// bottom (ring), then the line continues down (trail). The phases are adjacent
// and the ranges below feed strokeDashoffset / scaleY transforms directly, so
// the circle is not a second animation — it is the same stroke continuing, the
// ring literally drawn as an extension of the line.
const LEAD_END = 0.05
// The entrance always draws the FIRST half of the ring — top -> right -> bottom,
// clockwise. That half-circle is the running/pending state on its own. When a
// call resolves, the SECOND half draws (bottom -> left -> top) and closes the
// circle. The trail resumes where the first half ends, at the bottom.
const HALF_END = 0.45

// The reveal is a position sweep (a tip travelling down the line and around the
// ring), not a discrete transition — so it wants roughly uniform velocity, not
// the house ease-out. PANIT_DEFAULT_EASE front-loads so hard it would draw the
// whole ring in the first ~50ms and look instant; this near-symmetric ease-in-
// out gives every phase perceptible, evenly paced time.
const DRAW_EASE = [0.4, 0, 0.2, 1] as const

// When a new row streams in, the row above flips from last to not-last and its
// trailing padding grows over this window (see `transition-[padding]` /
// `duration-300` on the content columns below). That growth extends the line
// above down into the new row's slot, pushing the new row — node included —
// downward. A node that starts drawing during that window visibly drifts down.
// So the coordinator holds a streamed batch for exactly this long: the line
// finishes extending into place, THEN the reveal plays. (The initial batch has
// no row above it flipping, so it starts immediately.) Keep this in lockstep
// with the `duration-300` classes — if one changes, change the other.
const PUSH_SETTLE = 0.3

/**
 * The reveal coordinator. Rows enroll on mount; every enrollment from the same
 * commit is flushed (via microtask) as one batch. A batch plays as a SINGLE
 * `animate` call — total duration is the sum of the rows', eased once with
 * {@link DRAW_EASE} — and each row's progress is a linear slice of it. That is
 * what makes a message + tool call read as one stroke: there is literally one
 * easing window, so the tip crosses from a row's trail into the next row's
 * lead without decelerating. Batches are queued end to end (`busyUntil`), so
 * rows streaming in while a draw is still running wait their turn instead of
 * animating in parallel.
 */
function useRevealCoordinator(reduce: boolean) {
  type Entry = { progress: MotionValue<number>; duration: number; cancelled: boolean }
  const coord = React.useRef<{
    pending: Entry[]
    scheduled: boolean
    /** Timestamp (performance.now ms) when the current chain finishes drawing. */
    busyUntil: number
    hasPlayed: boolean
    active: Set<{ stop: () => void }>
  }>({ pending: [], scheduled: false, busyUntil: 0, hasPlayed: false, active: new Set() })

  React.useEffect(() => {
    const { active } = coord.current
    return () => active.forEach((controls) => controls.stop())
  }, [])

  return React.useCallback(
    (progress: MotionValue<number>, duration: number) => {
      const c = coord.current
      const entry: Entry = { progress, duration, cancelled: false }
      c.pending.push(entry)
      if (!c.scheduled) {
        c.scheduled = true
        // Effects for every row mounted in this commit run before the
        // microtask fires, so the whole commit lands in one batch, in DOM order.
        queueMicrotask(() => {
          c.scheduled = false
          const batch = c.pending.filter((e) => !e.cancelled)
          c.pending = []
          if (batch.length === 0) return

          const total = batch.reduce((sum, e) => sum + e.duration, 0)
          let at = 0
          const rows = batch.map((e) => {
            const start = at
            at += e.duration
            return { entry: e, start }
          })

          const now = performance.now()
          const settle = c.hasPlayed ? PUSH_SETTLE * 1000 : 0
          const startAt = Math.max(now + settle, c.busyUntil)
          c.busyUntil = startAt + total * 1000
          c.hasPlayed = true

          const controls = animate(0, 1, {
            duration: total,
            delay: (startAt - now) / 1000,
            ease: DRAW_EASE,
            onUpdate: (v) => {
              const t = v * total
              for (const r of rows) {
                if (r.entry.cancelled) continue
                r.entry.progress.set(
                  Math.min(1, Math.max(0, (t - r.start) / r.entry.duration))
                )
              }
            },
          })
          c.active.add(controls)
          controls.then(() => c.active.delete(controls))
        })
      }
      return () => {
        entry.cancelled = true
      }
    },
    // The coordinator is inert under reduced motion (rows never enroll), but a
    // preference change mid-session should not resurrect stale queue state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [reduce]
  )
}

/** One row's reveal: a shared progress value 0 -> 1, driven by the coordinator. */
function useRowProgress(reduce: boolean, { duration = 0.9 } = {}) {
  const { enroll } = useRun()
  const progress = useMotionValue(reduce ? 1 : 0)

  React.useEffect(() => {
    if (reduce) {
      progress.set(1)
      return
    }
    return enroll(progress, duration)
    // Runs once on mount — the row's entrance plays as it streams in.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  return progress
}

const stateStroke: Record<AgentStepState, string> = {
  pending: "var(--agent-line)",
  running: "var(--primary)",
  success: "var(--primary)",
  error: "var(--destructive)",
}

/**
 * The node's ring, drawn as ONE continuous clockwise stroke from the top.
 *
 * It is a single circle path — not two arcs — so there is exactly one start
 * point (12 o'clock) and no stray round-cap dot at the bottom. The drawn length
 * (`pathLength`) is the sum of two contributions: the row `progress` draws the
 * first half (top -> right -> bottom; this half alone is the running / pending
 * state), and `close` draws the second half (bottom -> left -> top) once the
 * call resolves. Both feed one length, so it reads as one clockwise sweep.
 */
function NodeRail({
  progress,
  close,
  state,
}: {
  progress: MotionValue<number>
  close: MotionValue<number>
  state: AgentStepState
}) {
  const { nodeSize: s, lineWidth: w } = useRun()
  const c = s / 2
  const r = c - w // keep the stroke inside the box

  const leadDraw = useTransform(progress, [0, LEAD_END], [0, 1])
  const firstHalf = useTransform(progress, [LEAD_END, HALF_END], [0, 0.5])
  const secondHalf = useTransform(close, [0, 1], [0, 0.5])
  const ringLen = useTransform([firstHalf, secondHalf], ([a, b]: number[]) => a + b)
  // Hide the path entirely until it has real length, so no round cap shows as a
  // dot at the start point before the draw begins.
  const ringOpacity = useTransform(ringLen, [0, 0.008], [0, 1])

  const top = c - r // 12 o'clock
  const bottom = c + r // 6 o'clock

  return (
    <svg
      aria-hidden
      className="absolute left-1/2 top-0 -translate-x-1/2"
      width={s}
      height={s}
      viewBox={`0 0 ${s} ${s}`}
      fill="none"
    >
      {/* Lead: short line into the top of the ring. Butt cap so its zero-length
          start renders nothing rather than a dot. */}
      <motion.path
        d={`M ${c} 0 L ${c} ${top}`}
        stroke="var(--agent-line)"
        strokeWidth={w}
        strokeLinecap="butt"
        style={{ pathLength: leadDraw }}
      />
      {/* The ring: one full circle, drawn clockwise from the top. */}
      <motion.path
        d={`M ${c} ${top} A ${r} ${r} 0 0 1 ${c} ${bottom} A ${r} ${r} 0 0 1 ${c} ${top}`}
        strokeWidth={w}
        strokeLinecap="round"
        className="transition-[stroke] duration-300"
        style={{ pathLength: ringLen, stroke: stateStroke[state], opacity: ringOpacity }}
      />
    </svg>
  )
}

/**
 * The indicator inside the ring. Running shows nothing — the half-drawn ring is
 * the loading state; pending shows a faint dot; resolved states show an icon.
 */
function StateGlyph({ state }: { state: AgentStepState }) {
  const { reduce } = useRun()
  return (
    <AnimatePresence mode="wait" initial={false}>
      <motion.span
        key={state}
        className="flex items-center justify-center"
        initial={reduce ? false : { scale: 0.4, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        exit={reduce ? undefined : { scale: 0.4, opacity: 0 }}
        transition={{ duration: 0.2, ease: PANIT_DEFAULT_EASE }}
      >
        {state === "success" ? (
          <Check className="h-[38%] w-[38%] text-primary" strokeWidth={2.5} />
        ) : state === "error" ? (
          <X className="h-[38%] w-[38%] text-destructive" strokeWidth={2.5} />
        ) : state === "pending" ? (
          <span
            className="block rounded-full bg-muted-foreground/45"
            style={{ width: "15%", height: "15%" }}
          />
        ) : null}
      </motion.span>
    </AnimatePresence>
  )
}

/** Drives the ring's second half: 1 once the call has resolved, else 0. */
function useCloseRing(
  state: AgentStepState,
  reduce: boolean,
  progress: MotionValue<number>
) {
  const isTerminal = state === "success" || state === "error"
  const close = useMotionValue(reduce ? (isTerminal ? 1 : 0) : 0)
  const firstRun = React.useRef(true)

  React.useEffect(() => {
    const terminal = state === "success" || state === "error"
    if (reduce) {
      close.set(terminal ? 1 : 0)
      firstRun.current = false
      return
    }
    const seal = () => animate(close, terminal ? 1 : 0, { duration: 0.5, ease: DRAW_EASE })
    // On a fresh mount that is already resolved, hold the second half until this
    // row's first half has actually drawn — the coordinator may queue the row
    // well after mount — so the whole ring reads as one continuous clockwise sweep.
    if (firstRun.current && terminal && progress.get() < HALF_END) {
      firstRun.current = false
      let controls: ReturnType<typeof animate> | undefined
      const unsub = progress.on("change", (v) => {
        if (v >= HALF_END) {
          unsub()
          controls = seal()
        }
      })
      return () => {
        unsub()
        controls?.stop()
      }
    }
    firstRun.current = false
    const controls = seal()
    return () => controls.stop()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state])

  return close
}

/** The ring plus the state indicator that sits inside it. */
function StepNode({
  progress,
  state,
}: {
  progress: MotionValue<number>
  state: AgentStepState
}) {
  const { nodeSize, reduce } = useRun()
  const close = useCloseRing(state, reduce, progress)

  // The icon settles in as the ring seals; the pending dot fades in with the row.
  const glyphFromClose = useTransform(close, [0.55, 1], [0, 1])
  const glyphFromProgress = useTransform(progress, [HALF_END, HALF_END + 0.15], [0, 1])
  const glyphOpacity = state === "pending" ? glyphFromProgress : glyphFromClose

  return (
    <>
      <NodeRail progress={progress} close={close} state={state} />
      <motion.div
        className="absolute left-1/2 -translate-x-1/2"
        style={{ top: 0, width: nodeSize, height: nodeSize }}
        // A brief nudge when a call fails, so an error reads as an event.
        animate={reduce || state !== "error" ? undefined : { x: [0, -3, 3, -2, 2, 0] }}
        transition={{ duration: 0.4, ease: PANIT_DEFAULT_EASE }}
      >
        <motion.div
          className="flex h-full w-full items-center justify-center"
          style={{ opacity: glyphOpacity }}
        >
          <StateGlyph state={state} />
        </motion.div>
      </motion.div>
    </>
  )
}

const useIsoLayoutEffect =
  typeof window !== "undefined" ? React.useLayoutEffect : React.useEffect

/** The straight length of line below a node (or the whole line for prose). */
function TrailLine({
  progress,
  from,
  range,
  isLast,
}: {
  progress: MotionValue<number>
  /** Distance from the top of the row where the line starts, in px. */
  from: number
  /** Progress window over which it draws. */
  range: [number, number]
  /** Final row — nothing connects below, so its length may lag the content. */
  isLast: boolean
}) {
  const { nodeSize, lineWidth, reduce } = useRun()
  const scaleY = useTransform(progress, range, [0, 1])

  // The line's length is the row's height minus its start offset — always driven
  // through ONE property, an explicit `height` on a MotionValue. We deliberately
  // never use `bottom: 0`: switching between a `bottom`-shaped style and a
  // `height`-shaped one leaves the MotionValue's `height` stuck on the element
  // (it wins over `bottom` in CSS), so a row that streamed as the last row and
  // then gained a row below it would freeze at its old, shorter height and
  // detach from the node beneath it.
  //
  // How the height is updated depends only on whether this is the last row:
  //   - A row with something BELOW it must stay pinned to the layout exactly, so
  //     the connector never detaches — we `set()` the measured height every frame
  //     (including through a padding push, which the ResizeObserver tracks).
  //   - The LAST row has nothing below it, so its length is free to lag. We use
  //     that to fix the streaming jump: when prose wraps, the row grows one
  //     line-height in a single frame and a pinned line would snap, so instead we
  //     EASE toward the new height. The text still snaps (you can't render half a
  //     line) but the line glides after it — and because only the last row eases,
  //     the lag can never open a gap above a node.
  const smooth = isLast && !reduce
  const ref = React.useRef<HTMLDivElement>(null)
  // Height is a MotionValue, never React state, and is written straight to the
  // DOM. That is deliberate: the reveal coordinator batches every row mounted in
  // one commit into a single stroke, so this must NOT trigger a render during
  // mount (a layout-effect setState would, and it desyncs a ring from the line
  // before it). The reveal owns `scaleY`, the length owns `height` — separate
  // properties on the same element, so they compose without fighting.
  const height = useMotionValue(0)

  useIsoLayoutEffect(() => {
    const el = ref.current?.parentElement
    if (!el) return
    let settled = false
    let controls: ReturnType<typeof animate> | undefined
    const measure = () => {
      const next = Math.max(0, el.getBoundingClientRect().height - from)
      // Non-last rows (and the first measure) land instantly, tracking the layout
      // exactly. Only a settled last row eases — its duration is in lockstep with
      // the `transition-[padding] duration-300` / PUSH_SETTLE used when rows push
      // each other down.
      if (!settled || !smooth) {
        settled = true
        controls?.stop()
        height.set(next)
      } else if (next !== height.get()) {
        controls?.stop()
        controls = animate(height, next, { duration: 0.3, ease: DRAW_EASE })
      }
    }
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => {
      ro.disconnect()
      controls?.stop()
    }
  }, [smooth, from])

  return (
    <motion.div
      ref={ref}
      aria-hidden
      className="absolute -translate-x-1/2 origin-top rounded-full bg-[var(--agent-line)]"
      style={{ left: nodeSize / 2, top: from, width: lineWidth, height, scaleY }}
    />
  )
}

/**
 * Data model for driving the timeline from state instead of hand-composed
 * children. Deliberately provider-agnostic: it carries only what the timeline
 * renders, in a shape any LLM API maps onto naturally — the app keeps its own
 * representation of the conversation (whatever its provider emits) and derives
 * these items from it. `message` items render as {@link AgentMessage}; `tool`
 * items as {@link AgentStep}.
 */
export type AgentRunMessageItem = {
  kind: "message"
  /** Stable identity — React key; keeps the reveal from replaying as text grows. */
  id: string
  /** The prose so far. Append as it streams; the row grows in place. */
  text: string
}

export type AgentRunToolItem = {
  kind: "tool"
  /** Stable identity — a tool-call id from the provider works well. */
  id: string
  /** The tool's name; the default title when {@link AgentRunProps.renderTool} doesn't override it. */
  name: string
  state: AgentStepState
  /** The call's arguments, in whatever shape the app has them. Only read by `renderTool`. */
  input?: unknown
  /** Result body shown under the header, set when the call resolves. */
  result?: React.ReactNode
}

export type AgentRunItem = AgentRunMessageItem | AgentRunToolItem

/** Presentation for one tool item; anything omitted falls back to a default. */
export type AgentToolRender = {
  title?: React.ReactNode
  meta?: React.ReactNode
  icon?: React.ComponentType<{ className?: string }>
  children?: React.ReactNode
}

type AgentRunProps = {
  /** Compose rows manually. Ignored when `items` is provided. */
  children?: React.ReactNode
  /** Data-driven mode: render these items in order. */
  items?: AgentRunItem[]
  /** Maps a tool item to its presentation. Defaults to the tool name as title. */
  renderTool?: (item: AgentRunToolItem) => AgentToolRender
  /** Outer node diameter in pixels. Defaults to 64 (4rem) — a deliberately spacious node. */
  nodeSize?: number
  /** Stroke width of the line and the node ring, in pixels. */
  lineWidth?: number
  className?: string
}

export function AgentRun({
  children,
  items,
  renderTool,
  nodeSize = 64,
  lineWidth = 2,
  className,
}: AgentRunProps) {
  const prefersReduced = useReducedMotion()
  const enroll = useRevealCoordinator(!!prefersReduced)
  const ctx = React.useMemo<RunContextValue>(
    () => ({ nodeSize, lineWidth, reduce: !!prefersReduced, enroll }),
    [nodeSize, lineWidth, prefersReduced, enroll]
  )

  const rows = items
    ? items.map((item) => {
        if (item.kind === "message") {
          return <AgentMessage key={item.id}>{item.text}</AgentMessage>
        }
        const r = renderTool?.(item) ?? {}
        return (
          <AgentStep
            key={item.id}
            state={item.state}
            title={r.title ?? item.name}
            meta={r.meta}
            icon={r.icon}
          >
            {r.children ?? item.result}
          </AgentStep>
        )
      })
    : React.Children.toArray(children)

  // Mark the last row so it does not reserve trailing connector space — the line
  // should end where content ends, not dangle past a still-running last call.
  const lastIndex = rows.length - 1

  return (
    <RunContext.Provider value={ctx}>
      <div
        className={cn("relative", className)}
        style={{ ["--agent-line" as string]: "var(--border)" }}
      >
        {rows.map((child, i) =>
          React.isValidElement(child)
            ? React.cloneElement(child as React.ReactElement<{ isLast?: boolean }>, {
                isLast: i === lastIndex,
              })
            : child
        )}
      </div>
    </RunContext.Provider>
  )
}


type AgentStepProps = {
  /** State of the tool call. Drives the inner disc and ring colour. */
  state: AgentStepState
  /** Short label — usually the tool name or action. */
  title: React.ReactNode
  /** Optional secondary line — target, duration, count. */
  meta?: React.ReactNode
  /** Optional leading icon for the tool, shown before the title. */
  icon?: React.ComponentType<{ className?: string }>
  /** Result / output body, revealed under the header. */
  children?: React.ReactNode
  className?: string
  /** @internal Set by {@link AgentRun}; true for the final row. */
  isLast?: boolean
}

/** A tool call on the timeline: a node on the line plus its header and output. */
export function AgentStep({
  state,
  title,
  meta,
  icon: Icon,
  children,
  className,
  isLast = false,
}: AgentStepProps) {
  const { nodeSize, lineWidth, reduce } = useRun()
  const progress = useRowProgress(reduce)
  const contentOpacity = useTransform(progress, [HALF_END, 0.9], [0, 1])
  const contentY = useTransform(progress, [HALF_END, 0.9], [6, 0])

  return (
    <div className={cn("relative flex gap-4", className)}>
      <div className="relative shrink-0" style={{ width: nodeSize }}>
        {/* The trail below the ring fills the gutter, which stretches to the
            content's height. On the last row there is no trailing space to
            reserve, so the line ends at the content rather than dangling. */}
        <TrailLine
          progress={progress}
          from={nodeSize - lineWidth}
          range={[HALF_END, 1]}
          isLast={isLast}
        />
        <StepNode progress={progress} state={state} />
      </div>

      <motion.div
        className={cn(
          "min-w-0 flex-1 transition-[padding] duration-300",
          isLast ? "pb-0" : "pb-6"
        )}
        style={{ minHeight: nodeSize, opacity: contentOpacity, y: contentY }}
      >
        <div className="flex flex-col justify-center" style={{ minHeight: nodeSize }}>
          <div className="flex items-center gap-2">
            {Icon ? <Icon className="h-4 w-4 shrink-0 text-muted-foreground" /> : null}
            <span
              className={cn(
                "truncate text-sm font-medium",
                state === "error" ? "text-destructive" : "text-foreground"
              )}
            >
              {title}
            </span>
          </div>
          {meta ? (
            <span className="mt-0.5 truncate text-xs text-muted-foreground">{meta}</span>
          ) : null}
        </div>

        {children ? (
          <div className="mt-1 text-sm text-muted-foreground">{children}</div>
        ) : null}
      </motion.div>
    </div>
  )
}

type AgentMessageProps = {
  children: React.ReactNode
  className?: string
  /** @internal Set by {@link AgentRun}; true for the final row. */
  isLast?: boolean
}

/** Agent prose on the timeline. The line passes through; there is no node. */
export function AgentMessage({ children, className, isLast = false }: AgentMessageProps) {
  const { nodeSize, reduce } = useRun()
  // Shorter slice than a step: prose has no ring to trace, just line + fade.
  const progress = useRowProgress(reduce, { duration: 0.55 })
  const opacity = useTransform(progress, [0.1, 0.8], [0, 1])
  const y = useTransform(progress, [0.1, 0.8], [6, 0])

  return (
    <div className={cn("relative flex gap-4", className)}>
      <div className="relative shrink-0" style={{ width: nodeSize }}>
        <TrailLine progress={progress} from={0} range={[0, 1]} isLast={isLast} />
      </div>
      <motion.div
        className={cn(
          "min-w-0 flex-1 text-sm leading-relaxed text-foreground transition-[padding] duration-300",
          isLast ? "pb-0" : "pb-6"
        )}
        style={{ opacity, y }}
      >
        {children}
      </motion.div>
    </div>
  )
}
