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
import { Check, ChevronRight, X } from "lucide-react"

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
 * {@link AgentThinking} is prose too, but behind a header the user can click:
 * its body shows fully, not at all, or as a peeking window onto the newest few
 * lines while reasoning is still streaming. The height is animated, and the
 * line beside it tracks that animation frame by frame, so the fold and the
 * timeline move together. A tool call folds the same way — its description,
 * arguments and output live behind its header, on the same machinery
 * ({@link useCollapsibleBody}).
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
  /**
   * True while the rows of {@link AgentRun}'s own first commit are mounting —
   * i.e. this row was already there when the timeline appeared, rather than
   * having streamed in. See {@link AgentRunProps.revealOnMount}.
   */
  isInitialCommit: () => boolean
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

  const enroll = React.useCallback(
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

  /**
   * Records that the timeline already had rows without any of them enrolling —
   * rows restored with {@link AgentRunProps.revealOnMount} off. The next row to
   * stream in still pushes the row above it (the `transition-[padding]` growth),
   * so it must wait out {@link PUSH_SETTLE} exactly as it would have if those
   * rows had drawn themselves.
   */
  const markPlayed = React.useCallback(() => {
    coord.current.hasPlayed = true
  }, [])

  return { enroll, markPlayed }
}

/**
 * One row's reveal: a shared progress value 0 -> 1, driven by the coordinator.
 *
 * A row that was already present when the timeline mounted is *settled*: it
 * never enrolls, and its progress starts and stays at 1. Restoring a stored
 * conversation is not an entrance — and because the coordinator's batch
 * duration is the SUM of its rows', letting a whole history enroll would spend
 * seconds redrawing a run the user has already seen.
 */
function useRowProgress(reduce: boolean, { duration = 0.9 } = {}) {
  const { enroll, isInitialCommit } = useRun()
  // Captured once per row, during its first render, so the value is stable and
  // the row's first paint is already correct — no frame of invisible content.
  const [settled] = React.useState(() => reduce || isInitialCommit())
  const progress = useMotionValue(settled ? 1 : 0)

  React.useEffect(() => {
    if (settled) {
      progress.set(1)
      return
    }
    return enroll(progress, duration)
    // Runs once on mount — the row's entrance plays as it streams in.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  return { progress, settled }
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
        // The stroke is a presentation attribute and must NOT move into `style`:
        // Motion seeds plain style entries into its own render state at mount
        // and owns the element from then on, so a colour sitting there never
        // repaints — a row created at `pending` when a tool call is announced
        // would keep the muted ring for the rest of the run. React owns the
        // attribute, so it is always the current state's. The
        // `transition-[stroke]` class still tweens between colours.
        stroke={stateStroke[state]}
        className="transition-[stroke] duration-300"
        style={{ pathLength: ringLen, opacity: ringOpacity }}
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
  progress: MotionValue<number>,
  /** The row never played an entrance, so its ring starts in its final shape. */
  settled: boolean
) {
  const isTerminal = state === "success" || state === "error"
  const close = useMotionValue(reduce || settled ? (isTerminal ? 1 : 0) : 0)
  const firstRun = React.useRef(true)

  React.useEffect(() => {
    const terminal = state === "success" || state === "error"
    if (reduce) {
      close.set(terminal ? 1 : 0)
      firstRun.current = false
      return
    }
    // A restored row is already in its final state; only a state change that
    // happens LATER — the call resolving under the user — is an event worth
    // animating, so this bails on the first run only.
    if (firstRun.current && settled) {
      firstRun.current = false
      close.set(terminal ? 1 : 0)
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
  settled,
}: {
  progress: MotionValue<number>
  state: AgentStepState
  settled: boolean
}) {
  const { nodeSize, reduce } = useRun()
  const close = useCloseRing(state, reduce, progress, settled)

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
  pinned = false,
  color = "var(--agent-line)",
}: {
  progress: MotionValue<number>
  /** Distance from the top of the row where the line starts, in px. */
  from: number
  /** Progress window over which it draws. */
  range: [number, number]
  /** Final row — nothing connects below, so its length may lag the content. */
  isLast: boolean
  /** The row is mid height-animation of its own — track it exactly, never ease. */
  pinned?: boolean
  /**
   * Colour of the line. Defaults to the timeline's own. A tool call hands its
   * state colour down while its body is open, so the stretch of line running
   * past the details reads as belonging to that call rather than as more
   * timeline — see {@link AgentStepImpl}.
   */
  color?: string
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
  //
  // `pinned` is the third case: the row is deliberately animating its own height
  // (a thinking block collapsing). The content is already gliding, so the
  // ResizeObserver fires every frame — easing on top of that would restart a
  // fresh tween per frame and leave the line rubber-banding behind the box, and
  // still easing after the collapse has landed. Exact tracking is what a row
  // that animates itself wants.
  const smooth = isLast && !reduce && !pinned
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
      // The colour crossfades on the same 300ms the ring's stroke uses, so a
      // call opening its body tints the line at the pace everything else moves.
      className="absolute origin-top -translate-x-1/2 rounded-full transition-[background-color] duration-300"
      style={{
        left: nodeSize / 2,
        top: from,
        width: lineWidth,
        height,
        scaleY,
        backgroundColor: color,
      }}
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
  /**
   * The call's arguments, in whatever shape the app has them. Only read by
   * `renderTool` — return them as `arguments` to show them in the folded body.
   */
  input?: unknown
  /** Result body, revealed when the row is expanded. Set when the call resolves. */
  result?: React.ReactNode
  /**
   * Controlled fold. Set it to drive the row from app state (and pair it with
   * {@link AgentRunProps.onToolOpenChange}) — that is how an app shows a call's
   * output while it runs and folds it away once it succeeds. Leave it undefined
   * to let the row own its own state, starting closed.
   */
  open?: boolean
}

export type AgentRunThinkingItem = {
  kind: "thinking"
  /** Stable identity — React key; keeps the reveal from replaying as text grows. */
  id: string
  /** The reasoning so far. Append as it streams; an open block grows in place. */
  text: string
  /** Header label. Defaults to "Thinking". */
  title?: React.ReactNode
  /**
   * Controlled mode. Set it to drive the block from app state (and pair it with
   * {@link AgentRunProps.onThinkingModeChange}) — that is how an app shows
   * reasoning as a peeking window while it streams and folds it away when the
   * turn moves on. Leave it undefined to let the block own its own mode,
   * starting collapsed.
   */
  mode?: AgentThinkingMode
}

export type AgentRunItem =
  | AgentRunMessageItem
  | AgentRunToolItem
  | AgentRunThinkingItem

/** Presentation for one tool item; anything omitted falls back to a default. */
export type AgentToolRender = {
  title?: React.ReactNode
  meta?: React.ReactNode
  icon?: React.ComponentType<{ className?: string }>
  /** Prose describing the call, shown first in the expanded body. */
  description?: React.ReactNode
  /** The call's arguments, rendered in a monospace block. */
  arguments?: React.ReactNode
  /** Output body. Defaults to the item's `result`. */
  children?: React.ReactNode
}

type AgentRunProps = {
  /** Compose rows manually. Ignored when `items` is provided. */
  children?: React.ReactNode
  /** Data-driven mode: render these items in order. */
  items?: AgentRunItem[]
  /** Maps a tool item to its presentation. Defaults to the tool name as title. */
  renderTool?: (item: AgentRunToolItem) => AgentToolRender
  /**
   * Fires when the user clicks a thinking item's header, with the mode the
   * component would pick on its own. Required only to *control* the mode from
   * app state (write a value back onto the item's `mode`) — which is what lets
   * the app override the default, e.g. sending a click back to `"peek"` instead
   * of `"collapsed"` while the model is still streaming. An item without `mode`
   * manages itself and still reports here.
   */
  onThinkingModeChange?: (id: string, mode: AgentThinkingMode) => void
  /**
   * Fires when the user clicks a tool item's header, with the fold state the
   * click implies. Required only to *control* the fold from app state (write
   * the value back onto the item's `open`); an item without `open` manages
   * itself and still reports here.
   */
  onToolOpenChange?: (id: string, open: boolean) => void
  /**
   * Whether rows that are already present when the timeline mounts play the
   * reveal. Defaults to `true` — a run that starts empty and streams in draws
   * itself, which is the point of the component.
   *
   * Turn it off when the rows are HISTORY rather than an entrance: restoring a
   * stored session, switching to another conversation, navigating back. A batch
   * costs the SUM of its rows' durations, so a restored turn otherwise spends
   * seconds redrawing itself before the last row is on screen. Rows that stream
   * in afterwards still reveal normally either way.
   */
  revealOnMount?: boolean
  /** Outer node diameter in pixels. Defaults to 40. */
  nodeSize?: number
  /** Stroke width of the line and the node ring, in pixels. */
  lineWidth?: number
  className?: string
}

/**
 * Per-item callbacks that stay identical across renders, so a row is not
 * re-rendered — and its body, often a whole rendered document, re-rendered with
 * it — every time an unrelated item in the list changes. In `items` mode that
 * happens on every streamed token.
 */
function useItemHandler<T>(callback: ((id: string, value: T) => void) | undefined) {
  const handlers = React.useRef(new Map<string, (value: T) => void>())
  const latest = React.useRef(callback)
  latest.current = callback
  return React.useCallback((id: string) => {
    let handler = handlers.current.get(id)
    if (!handler) {
      handler = (value: T) => latest.current?.(id, value)
      handlers.current.set(id, handler)
    }
    return handler
  }, [])
}

export function AgentRun({
  children,
  items,
  renderTool,
  onThinkingModeChange,
  onToolOpenChange,
  revealOnMount = true,
  nodeSize = 40,
  lineWidth = 1.5,
  className,
}: AgentRunProps) {
  const prefersReduced = useReducedMotion()
  const { enroll, markPlayed } = useRevealCoordinator(!!prefersReduced)

  // True only while this commit's rows are mounting. Row effects run before the
  // parent's, so a row that mounts with the timeline reads `true` here and one
  // that streams in later reads `false` — which is exactly the distinction
  // `revealOnMount` draws.
  const initialCommit = React.useRef(true)
  const hadInitialRows = React.useRef(false)
  const isInitialCommit = React.useCallback(
    () => !revealOnMount && initialCommit.current,
    [revealOnMount]
  )

  React.useEffect(() => {
    initialCommit.current = false
    // Settled rows never enrolled, so no batch has flushed to set `hasPlayed` —
    // but the first row to stream in still pushes the last of them down, so the
    // coordinator has to know the timeline is not starting from empty. When the
    // initial rows DID reveal, their own batch sets this, and doing it here
    // instead would wrongly charge that first batch a PUSH_SETTLE delay.
    if (!revealOnMount && hadInitialRows.current) markPlayed()
  }, [markPlayed, revealOnMount])

  const ctx = React.useMemo<RunContextValue>(
    () => ({ nodeSize, lineWidth, reduce: !!prefersReduced, enroll, isInitialCommit }),
    [nodeSize, lineWidth, prefersReduced, enroll, isInitialCommit]
  )

  const modeHandler = useItemHandler(onThinkingModeChange)
  const openHandler = useItemHandler(onToolOpenChange)

  const rows = items
    ? items.map((item) => {
        if (item.kind === "message") {
          return <AgentMessage key={item.id}>{item.text}</AgentMessage>
        }
        if (item.kind === "thinking") {
          return (
            <AgentThinking
              key={item.id}
              title={item.title}
              mode={item.mode}
              onModeChange={modeHandler(item.id)}
            >
              {item.text}
            </AgentThinking>
          )
        }
        const r = renderTool?.(item) ?? {}
        return (
          <AgentStep
            key={item.id}
            state={item.state}
            title={r.title ?? item.name}
            meta={r.meta}
            icon={r.icon}
            description={r.description}
            arguments={r.arguments}
            open={item.open}
            onOpenChange={openHandler(item.id)}
          >
            {r.children ?? item.result}
          </AgentStep>
        )
      })
    : React.Children.toArray(children)

  if (initialCommit.current) hadInitialRows.current = rows.length > 0

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
  /** Optional secondary line — target, duration, count. Always visible. */
  meta?: React.ReactNode
  /** Optional leading icon for the tool, shown before the title. */
  icon?: React.ComponentType<{ className?: string }>
  /** What the call does, in prose. First section of the expanded body. */
  description?: React.ReactNode
  /**
   * The call's arguments, shown in a monospace block. Pass whatever the app
   * wants read — a formatted JSON string is the usual choice.
   */
  arguments?: React.ReactNode
  /** Result / output body. The last section of the expanded body. */
  children?: React.ReactNode
  /**
   * Controlled fold. Pass it (with `onOpenChange`) to own whether the body
   * shows — which is how an app opens a call while it runs and folds it away
   * once it succeeds. Omit it and the row manages its own, starting at
   * `defaultOpen`.
   */
  open?: boolean
  /** Initial fold state when uncontrolled. Defaults to `false`. */
  defaultOpen?: boolean
  /** Fires on every header click, controlled or not, with the state a click implies. */
  onOpenChange?: (open: boolean) => void
  className?: string
  /**
   * Background applied to the header on hover/focus. Defaults to a stronger
   * wash than the rest of the registry's `hover:bg-muted/40` convention,
   * because `--muted` sits so close to the page background that the usual
   * opacity reads as invisible against an app's own ambient gradients — see
   * the header's `header` render below.
   */
  hoverClassName?: string
  /** @internal Set by {@link AgentRun}; true for the final row. */
  isLast?: boolean
}

/** True for anything React would actually paint. */
function isPresent(node: React.ReactNode) {
  return node !== null && node !== undefined && node !== false && node !== ""
}

/** A labelled section of an expanded tool call. */
function StepSection({
  label,
  mono = false,
  children,
}: {
  label: string
  mono?: boolean
  children: React.ReactNode
}) {
  return (
    <div>
      <div className="mb-1 text-[10px] font-medium tracking-wide text-muted-foreground/70 uppercase">
        {label}
      </div>
      {mono ? (
        <pre className="overflow-x-auto rounded-md border border-border bg-muted/40 px-3 py-2 font-mono text-xs whitespace-pre-wrap text-muted-foreground">
          {children}
        </pre>
      ) : (
        <div className="text-sm text-muted-foreground">{children}</div>
      )}
    </div>
  )
}

/**
 * A tool call on the timeline: a node on the line plus its header, and — when
 * there is anything to show — a body the user can fold open.
 *
 * The header stays in the node-height box so it never drifts away from the ring
 * beside it; everything expansible hangs below that box, on the same viewport
 * driver a thinking block uses ({@link useCollapsibleBody}), so the height
 * animates and the timeline's line tracks it frame by frame.
 */
function AgentStepImpl({
  state,
  title,
  meta,
  icon: Icon,
  description,
  arguments: args,
  children,
  open,
  defaultOpen = false,
  onOpenChange,
  className,
  hoverClassName = "hover:bg-accent focus-visible:bg-accent",
  isLast = false,
}: AgentStepProps) {
  const { nodeSize, lineWidth, reduce } = useRun()
  const { progress, settled } = useRowProgress(reduce)
  const contentOpacity = useTransform(progress, [HALF_END, 0.9], [0, 1])
  const contentY = useTransform(progress, [HALF_END, 0.9], [6, 0])

  const [uncontrolled, setUncontrolled] = React.useState(defaultOpen)
  const current = open ?? uncontrolled
  const bodyId = React.useId()

  const expansible =
    isPresent(description) || isPresent(args) || isPresent(children)
  // A row with nothing folded away must not claim to be open, or its clip box
  // would measure to zero and the chevron would lie.
  const isOpen = expansible && current

  const toggle = () => {
    if (open === undefined) setUncontrolled(!current)
    onOpenChange?.(!current)
  }

  // A tool call never peeks — it is open or it is not — so the peek window size
  // is moot here.
  const { clipRef, contentRef, height, mask, toggling } = useCollapsibleBody(
    isOpen ? "expanded" : "collapsed",
    reduce,
    0
  )

  const header = (
    <>
      {/* A span, not a div: this also renders inside a <button>, which only
          admits phrasing content. */}
      <span className="flex items-center gap-2">
        {Icon ? <Icon className="h-4 w-4 shrink-0 text-muted-foreground" /> : null}
        <span
          className={cn(
            "truncate text-sm font-medium",
            state === "error" ? "text-destructive" : "text-foreground"
          )}
        >
          {title}
        </span>
        {expansible ? (
          <ChevronRight
            className={cn(
              "ml-auto h-3.5 w-3.5 shrink-0 text-muted-foreground",
              !reduce && "transition-transform duration-300",
              isOpen && "rotate-90"
            )}
          />
        ) : null}
      </span>
      {meta ? (
        <span className="mt-0.5 block truncate text-xs text-muted-foreground">
          {meta}
        </span>
      ) : null}
    </>
  )

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
          pinned={toggling}
          // Open, the line past the details carries the call's own colour — the
          // ring's stroke continuing down the body it belongs to, so an expanded
          // block reads as part of the call and not as another row.
          color={isOpen ? stateStroke[state] : undefined}
        />
        <StepNode progress={progress} state={state} settled={settled} />
      </div>

      <motion.div
        className={cn(
          "min-w-0 flex-1 transition-[padding] duration-300",
          isLast ? "pb-0" : "pb-6"
        )}
        style={{ minHeight: nodeSize, opacity: contentOpacity, y: contentY }}
      >
        {/* The header keeps the node's height whether the body is open or not,
            so the title stays level with the ring instead of riding the fold. */}
        {expansible ? (
          <button
            type="button"
            onClick={toggle}
            aria-expanded={isOpen}
            aria-controls={bodyId}
            // The width is spelled out because a button sizes to its content
            // even as a flex container — without it the chevron would sit next
            // to the title instead of at the row's edge, and `truncate` would
            // have nothing to truncate against. The extra 0.5rem is the negative
            // margin, so the hit area and focus ring reach past the text on both
            // sides while the icon stays aligned with the rows that have no fold.
            className={cn(
              "-mx-1 flex w-[calc(100%_+_0.5rem)] flex-col justify-center rounded-md px-1 text-left outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50",
              hoverClassName
            )}
            style={{ minHeight: nodeSize }}
          >
            {header}
          </button>
        ) : (
          <div className="flex flex-col justify-center" style={{ minHeight: nodeSize }}>
            {header}
          </div>
        )}

        {/* The clip box, as in AgentThinking: `overflow: hidden` rides the
            animated element itself, and the text stays in the DOM at height 0,
            so it is hidden from assistive tech while folded away. */}
        <motion.div
          ref={clipRef}
          aria-hidden={!isOpen}
          style={{ height, overflow: "hidden", maskImage: mask, WebkitMaskImage: mask }}
        >
          <div
            id={bodyId}
            ref={contentRef}
            // `flow-root` keeps the padding inside the measured height rather
            // than letting a child's margin collapse out of it.
            className="pt-2"
            style={{ display: "flow-root" }}
          >
            <div className="flex flex-col gap-3">
              {isPresent(description) ? (
                <p className="text-sm text-muted-foreground">{description}</p>
              ) : null}
              {isPresent(args) ? (
                <StepSection label="Arguments" mono>
                  {args}
                </StepSection>
              ) : null}
              {isPresent(children) ? (
                <StepSection label="Output">{children}</StepSection>
              ) : null}
            </div>
          </div>
        </motion.div>
      </motion.div>
    </div>
  )
}

// Rows are memoized because a row's body is whatever the app renders into it —
// often a full markdown document. In `items` mode the timeline re-renders on
// every streamed token, and without this each of those renders would walk every
// other row's body as well.
export const AgentStep = React.memo(AgentStepImpl)
AgentStep.displayName = "AgentStep"

type AgentMessageProps = {
  children: React.ReactNode
  className?: string
  /** @internal Set by {@link AgentRun}; true for the final row. */
  isLast?: boolean
}

/** Agent prose on the timeline. The line passes through; there is no node. */
function AgentMessageImpl({ children, className, isLast = false }: AgentMessageProps) {
  const { nodeSize, reduce } = useRun()
  // Shorter slice than a step: prose has no ring to trace, just line + fade.
  const { progress } = useRowProgress(reduce, { duration: 0.55 })
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

export const AgentMessage = React.memo(AgentMessageImpl)
AgentMessage.displayName = "AgentMessage"

// How long a foldable body takes to move between modes.
const COLLAPSE_DURATION = 0.3

// Height of the fade at the top of a peeking block, in px.
const PEEK_FADE_PX = 40

/**
 * How much of a foldable body is showing.
 *
 * - `collapsed` — header only.
 * - `peek` — a fixed window onto the LAST few lines, pinned to the newest one
 *   and faded out at the top. This is the shape reasoning wants *while it is
 *   still arriving*: the text keeps moving, but the row's height does not.
 * - `expanded` — the whole thing.
 *
 * A thinking block uses all three; a tool call only ever folds between
 * `collapsed` and `expanded`.
 */
export type AgentThinkingMode = "collapsed" | "peek" | "expanded"

/**
 * Viewport driver for a foldable body — {@link AgentThinking}'s reasoning and
 * {@link AgentStep}'s description / arguments / output alike: how tall the clip
 * box is, how strong the top fade is, and where it is scrolled.
 *
 * The steady-state height is NOT an animation target — it is `set()` straight
 * from the measurement whenever the content changes. That matters because
 * thinking text streams: if the open state merely aimed a tween at the current
 * height, every appended token would restart it and the box would ease along
 * behind the text, clipping the newest line the whole time it was arriving.
 *
 * A tween runs only for a MODE CHANGE. Content that changes mid-tween — a line
 * wrapping while an expand is still playing, which is likely at streaming
 * speed — retargets the tween into the time it has LEFT rather than restarting
 * it. That keeps the whole transition inside one 0.3s budget, so it can never
 * chase the stream, while still ruling out a visible jump mid-expand.
 *
 * `peek` sidesteps that entirely: its height is capped, so streaming does not
 * move it at all — only `scrollTop` advances, which is why the mode exists.
 *
 * Both values are MotionValues written straight to the DOM (never React state),
 * for the same reason {@link TrailLine} does it: a measurement must not schedule
 * a render, or a row mounting mid-batch would desync from the reveal stroke.
 *
 * @returns `toggling` — true only while a mode tween is in flight. The row hands
 * it to {@link TrailLine} as `pinned` so the line tracks the change exactly
 * instead of easing after it.
 */
function useCollapsibleBody(
  mode: AgentThinkingMode,
  reduce: boolean,
  peekLines: number
) {
  const clipRef = React.useRef<HTMLDivElement>(null)
  const contentRef = React.useRef<HTMLDivElement>(null)
  const height = useMotionValue(0)
  const fade = useMotionValue(0)
  const [toggling, setToggling] = React.useState(false)
  const controls = React.useRef<ReturnType<typeof animate>[]>([])
  // Read inside the ResizeObserver — refs, not deps, so it never re-subscribes.
  const modeRef = React.useRef(mode)
  const linesRef = React.useRef(peekLines)
  linesRef.current = peekLines
  const target = React.useRef({ h: 0, f: 0 })
  const firstRun = React.useRef(true)

  /**
   * Keep the newest line in view. Programmatic scroll works on an
   * overflow-hidden box, so the window follows the stream without ever becoming
   * a scrollbar.
   *
   * Only scroll when the content is genuinely taller than the window we are
   * aiming at, and judge that against the TARGET height rather than the
   * rendered one. `height.set()` does not reach the DOM synchronously — motion
   * writes MotionValues in its own frame step — so a block that is still
   * shorter than the peek window would otherwise be scrolled to the bottom of
   * its previous, one-line-shorter self on the frame a line wraps: the text
   * jumps up, and the next frame (once the new height lands) puts it back.
   *
   * Only `peek` has a window to keep pinned, and this runs on EVERY frame of a
   * mode tween (it is subscribed to `height`). So it must bail for the other two
   * modes before it reads layout: `expanded` shows everything and `collapsed`
   * has no height at all, and the two reads below — `getBoundingClientRect` on
   * the content and `scrollHeight` on the clip box — are not free when the body
   * is a large rendered document.
   */
  const pin = React.useCallback(() => {
    const clip = clipRef.current
    const el = contentRef.current
    if (!clip || !el || modeRef.current !== "peek") return
    if (el.getBoundingClientRect().height <= target.current.h + 1) return
    clip.scrollTop = clip.scrollHeight
  }, [])

  /** The clip box's height and fade for the current mode and content. */
  const compute = React.useCallback(() => {
    const el = contentRef.current
    if (!el) return { h: 0, f: 0 }
    if (modeRef.current === "collapsed") return { h: 0, f: 0 }
    const full = el.getBoundingClientRect().height
    if (modeRef.current === "expanded") return { h: full, f: 0 }
    const cs = getComputedStyle(el)
    const line = parseFloat(cs.lineHeight) || 22
    const window = line * linesRef.current + (parseFloat(cs.paddingTop) || 0)
    // Fade only once there is something above the window to fade out.
    return { h: Math.min(full, window), f: full > window + 1 ? 1 : 0 }
  }, [])

  const stop = React.useCallback(() => {
    controls.current.forEach((c) => c.stop())
    controls.current = []
  }, [])

  const settle = React.useCallback(
    (next: { h: number; f: number }) => {
      stop()
      setToggling(false)
      height.set(next.h)
      fade.set(next.f)
      pin()
    },
    [fade, height, pin, stop]
  )

  /** When the in-flight mode transition started, so a retarget can use what is
   *  left of its budget instead of granting itself a fresh one. */
  const startedAt = React.useRef(0)

  const play = React.useCallback(
    (next: { h: number; f: number }, duration: number) => {
      stop()
      setToggling(true)
      const opts = { duration, ease: PANIT_DEFAULT_EASE }
      const h = animate(height, next.h, opts)
      const f = animate(fade, next.f, opts)
      controls.current = [h, f]
      h.then(() => {
        // A newer tween may have superseded this one; only the live one settles.
        if (!controls.current.includes(h)) return
        controls.current = []
        setToggling(false)
        pin()
      })
    },
    [fade, height, pin, stop]
  )

  useIsoLayoutEffect(() => {
    const el = contentRef.current
    if (!el) return
    const measure = () => {
      const next = compute()
      const changed = next.h !== target.current.h || next.f !== target.current.f
      target.current = next
      if (controls.current.length > 0) {
        // Mid-tween. Let it finish when it is still heading somewhere valid;
        // when the content moved the goalposts, aim at the new target within
        // the time this transition had left.
        if (!changed) return
        const left = COLLAPSE_DURATION - (performance.now() - startedAt.current) / 1000
        play(next, Math.max(0.05, left))
      } else {
        height.set(next.h)
        fade.set(next.f)
      }
      pin()
    }
    measure()
    const ro = new ResizeObserver(measure)
    // Height changes every frame of a mode tween; re-pin so the window keeps
    // showing the tail as it grows or shrinks.
    const unsub = height.on("change", pin)
    ro.observe(el)
    return () => {
      ro.disconnect()
      unsub()
    }
  }, [compute, fade, height, pin, play])

  React.useEffect(() => {
    modeRef.current = mode
    const next = compute()
    target.current = next
    // The initial state is a rendering, not a transition: however the row first
    // appears, it is already in that state on the first paint.
    if (firstRun.current || reduce) {
      firstRun.current = false
      settle(next)
      return
    }
    startedAt.current = performance.now()
    play(next, COLLAPSE_DURATION)
  }, [mode, reduce, compute, settle, play])

  React.useEffect(() => () => controls.current.forEach((c) => c.stop()), [])

  // Fades the top edge of the window out, so a cut-off block reads as cut off
  // rather than as text that happens to start mid-sentence. A mask rather than a
  // gradient overlay: it fades the text against whatever surface is behind the
  // row, with no assumption about the background colour.
  const mask = useTransform(fade, (f) =>
    f <= 0.001
      ? "none"
      : `linear-gradient(to bottom, rgba(0,0,0,${(1 - f).toFixed(3)}) 0px, rgb(0,0,0) ${(
          f * PEEK_FADE_PX
        ).toFixed(1)}px)`
  )

  return { clipRef, contentRef, height, mask, toggling }
}

type AgentThinkingProps = {
  /** The reasoning text. Append as it streams; the block keeps up in every mode. */
  children: React.ReactNode
  /** Header label, always visible. Defaults to "Thinking". */
  title?: React.ReactNode
  /**
   * Controlled mode. Pass it (with `onModeChange`) to own how much shows —
   * which is what lets the app do things the component cannot know to do, like
   * dropping back to `peek` when a click lands while the model is still
   * streaming. Omit it and the block manages its own, starting at
   * `defaultMode`.
   */
  mode?: AgentThinkingMode
  /** Initial mode when uncontrolled. Defaults to `"collapsed"`. */
  defaultMode?: AgentThinkingMode
  /**
   * Fires on every header click, controlled or not, with the mode the component
   * would move to on its own: a click opens a `collapsed` or `peek` block fully,
   * and closes an `expanded` one. Ignore it and set whatever mode you want.
   */
  onModeChange?: (mode: AgentThinkingMode) => void
  /** Lines of reasoning kept visible in `peek`. Defaults to 6. */
  peekLines?: number
  className?: string
  /** @internal Set by {@link AgentRun}; true for the final row. */
  isLast?: boolean
}

/** What a header click does when the component is left to decide for itself. */
function nextMode(mode: AgentThinkingMode): AgentThinkingMode {
  return mode === "expanded" ? "collapsed" : "expanded"
}

/**
 * A block of agent reasoning on the timeline: a header the user can click, and
 * no node — like {@link AgentMessage}, the line simply passes through. The body
 * shows fully, as a peeking window onto the newest lines, or not at all; see
 * {@link AgentThinkingMode}. The header sits OUTSIDE the clipped body, so its
 * focus ring is never shaved by the height animation.
 */
function AgentThinkingImpl({
  children,
  title = "Thinking",
  mode,
  defaultMode = "collapsed",
  onModeChange,
  peekLines = 6,
  className,
  isLast = false,
}: AgentThinkingProps) {
  const { nodeSize, reduce } = useRun()
  // Same slice as prose — a thinking row is part of the same stroke chain, and
  // its reveal is mount-only, so changing mode never re-enrolls with the
  // coordinator.
  const { progress } = useRowProgress(reduce, { duration: 0.55 })
  const opacity = useTransform(progress, [0.1, 0.8], [0, 1])
  const y = useTransform(progress, [0.1, 0.8], [6, 0])

  const [uncontrolled, setUncontrolled] = React.useState(defaultMode)
  const current = mode ?? uncontrolled
  const bodyId = React.useId()

  const toggle = () => {
    const next = nextMode(current)
    if (mode === undefined) setUncontrolled(next)
    onModeChange?.(next)
  }

  const { clipRef, contentRef, height, mask, toggling } = useCollapsibleBody(
    current,
    reduce,
    peekLines
  )

  return (
    <div className={cn("relative flex gap-4", className)}>
      <div className="relative shrink-0" style={{ width: nodeSize }}>
        <TrailLine
          progress={progress}
          from={0}
          range={[0, 1]}
          isLast={isLast}
          pinned={toggling}
        />
      </div>
      <motion.div
        className={cn(
          "min-w-0 flex-1 transition-[padding] duration-300",
          isLast ? "pb-0" : "pb-6"
        )}
        style={{ opacity, y }}
      >
        <button
          type="button"
          onClick={toggle}
          aria-expanded={current !== "collapsed"}
          aria-controls={bodyId}
          className="-mx-1 flex items-center gap-1.5 rounded-md px-1 py-0.5 text-sm text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
        >
          <ChevronRight
            className={cn(
              "h-3.5 w-3.5 shrink-0",
              !reduce && "transition-transform duration-300",
              current !== "collapsed" && "rotate-90"
            )}
          />
          <span className="truncate">{title}</span>
        </button>
        {/* The clip box. `overflow: hidden` belongs on the animated element
            itself — it is what hides the excess mid-transition — and the mask
            has to ride the same box it clips. Hidden from assistive tech when
            collapsed: the text is still in the DOM at height 0. */}
        <motion.div
          ref={clipRef}
          aria-hidden={current === "collapsed"}
          style={{
            height,
            overflow: "hidden",
            maskImage: mask,
            WebkitMaskImage: mask,
          }}
        >
          <div
            id={bodyId}
            ref={contentRef}
            // `flow-root` keeps the padding inside the measured height rather
            // than letting a child's margin collapse out of it.
            className="pt-1 pl-[1.375rem] text-sm leading-relaxed text-muted-foreground"
            style={{ display: "flow-root" }}
          >
            {children}
          </div>
        </motion.div>
      </motion.div>
    </div>
  )
}

export const AgentThinking = React.memo(AgentThinkingImpl)
AgentThinking.displayName = "AgentThinking"
