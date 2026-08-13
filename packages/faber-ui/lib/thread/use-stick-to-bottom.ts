"use client"

import * as React from "react"

/**
 * How close to the bottom (px) still counts as being there.
 *
 * Deliberately smaller than one wheel notch (~100px): it is what decides
 * whether a user who has scrolled counts as having come back, so a threshold
 * as tall as a notch would re-arm autoscroll on the very gesture that was
 * meant to leave. Not near zero either — content grows while the user reads,
 * so the bottom keeps receding and an exact landing is not something to ask
 * for.
 */
const AT_BOTTOM_PX = 32

/**
 * How long to keep treating scroll events as our own smooth scroll's.
 *
 * The browser owns the animation and does not say when it stopped — `scrollend`
 * is not everywhere yet, and an animation cut short by a scrollbar drag may
 * never reach the bottom to announce itself that way either. So the flag is
 * given a lifetime instead: comfortably longer than the ~300ms the animation
 * takes, short enough that a stuck one cannot hold the release rule hostage.
 */
const GLIDE_MS = 1000

/**
 * Read at pin time rather than through `useReducedMotion`: the pin is an
 * imperative one-off, and a hook would make the preference a dependency of the
 * effect — one that resolves a beat after mount and would re-run the pin for
 * no reason.
 */
function prefersReducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
}

export type StickToBottom<T extends HTMLElement> = {
  /** Goes on the scroll container. */
  ref: React.RefObject<T | null>
  /** Goes on the same container's `onScroll`. */
  onScroll: () => void
  /** Re-arms autoscroll — sending a message is a request to follow along again. */
  stick: () => void
}

/**
 * Keeps a scroll container pinned to the bottom, until the user scrolls away —
 * and then leaves them alone.
 *
 * `content` is what a pin is worth doing for, and its granularity is the
 * caller's to choose: pass something that changes when a row arrives and the
 * view glides once per row, rather than riding every token that fills one in.
 *
 * Pins are ANIMATED, which is the other reason granularity matters: a glide
 * reads as the page following the run, where the same motion per token would be
 * a permanent slow drift. A pin jumps instead when the movement is not
 * something the user could follow anyway — opening a thread at its end, or
 * closing a gap longer than the viewport — and under `prefers-reduced-motion`.
 *
 * The releasing gesture is read from the INPUT (wheel, touch), not from the
 * resulting scroll position, and that is the whole point of this hook. A
 * scroll handler sees position at frame time and has to read it live from the
 * DOM; while a run streams, every delta re-pins `scrollTop` to the bottom in
 * between. So by the time a handler asks where the user scrolled to, the
 * answer is already "the bottom" — their gesture was overwritten before
 * anything could observe it, which is why position alone can never notice
 * someone trying to leave. Flagging the pin and ignoring its own scroll event
 * does not help: the user's event reads the same overwritten position.
 *
 * Input events fire before any of that. `wheel` up or a downward drag means
 * leaving, full stop, whatever the position says afterwards.
 *
 * Position is still what brings them back: once released nothing re-pins, so
 * the container's own scroll events are honest again, and reaching the bottom
 * re-arms. The one position the rule must not believe is a frame of our own
 * glide, every one of which is short of the bottom by construction — hence
 * `gliding`, which suspends the rule for the length of the animation and no
 * longer.
 *
 * Keyboard scrolling is not handled, because the container is not focusable —
 * arrow and page keys never reach it, focus being on the prompt box or the
 * body. Scrolling by dragging the scrollbar is covered by the position rule
 * rather than by an intent listener of its own.
 */
export function useStickToBottom<T extends HTMLElement>(content: unknown): StickToBottom<T> {
  const ref = React.useRef<T>(null)
  const sticking = React.useRef(true)

  // True while a smooth scroll of ours is in flight. Everything the animation
  // does to `scrollTop` is our own doing, so the position rule has to sit it
  // out — see `onScroll`.
  const gliding = React.useRef(false)
  const glideTimer = React.useRef<ReturnType<typeof setTimeout> | null>(null)

  // A transcript opens at its end; it does not scroll there in front of the
  // user. Only the pins that follow — a row arriving in a thread they are
  // already reading — are movement worth animating.
  const opened = React.useRef(false)

  React.useEffect(() => {
    const el = ref.current
    if (!el || !sticking.current) return

    // Distance nobody can follow is not worth animating either: a message sent
    // from far up a long thread should arrive at the bottom, not travel there.
    const gap = el.scrollHeight - el.clientHeight - el.scrollTop
    const smooth = opened.current && gap <= el.clientHeight && !prefersReducedMotion()

    // A pin with nothing to scroll is not the opening — it is the mount, before
    // the transcript has loaded. Letting it spend the jump would leave the
    // history itself to be animated in when it arrives.
    if (el.scrollHeight > el.clientHeight) opened.current = true

    if (!smooth) {
      el.scrollTop = el.scrollHeight
      return
    }

    gliding.current = true
    if (glideTimer.current) clearTimeout(glideTimer.current)
    glideTimer.current = setTimeout(() => {
      gliding.current = false
    }, GLIDE_MS)

    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" })
  }, [content])

  React.useEffect(() => () => {
    if (glideTimer.current) clearTimeout(glideTimer.current)
  }, [])

  React.useEffect(() => {
    const el = ref.current
    if (!el) return

    const release = () => {
      sticking.current = false
      // Stop mid-glide rather than fight the user for the rest of it: a scroll
      // to where the container already is cancels the browser's animation.
      if (gliding.current) {
        gliding.current = false
        el.scrollTo({ top: el.scrollTop, behavior: "auto" })
      }
    }

    const onWheel = (event: WheelEvent) => {
      if (event.deltaY < 0) release()
    }

    // A finger moving DOWN the screen scrolls the content up.
    let touchY: number | null = null
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY ?? null
    }
    const onTouchMove = (event: TouchEvent) => {
      const y = event.touches[0]?.clientY
      if (y === undefined) return
      if (touchY !== null && y > touchY) release()
      touchY = y
    }

    // Listen-only: none of these call `preventDefault`, and a non-passive
    // wheel listener on a scroll container is a scroll-performance cost for
    // nothing.
    const options = { passive: true } as const
    el.addEventListener("wheel", onWheel, options)
    el.addEventListener("touchstart", onTouchStart, options)
    el.addEventListener("touchmove", onTouchMove, options)

    return () => {
      el.removeEventListener("wheel", onWheel)
      el.removeEventListener("touchstart", onTouchStart)
      el.removeEventListener("touchmove", onTouchMove)
    }
  }, [])

  const onScroll = React.useCallback(() => {
    const el = ref.current
    if (!el) return

    if (el.scrollHeight - el.scrollTop - el.clientHeight < AT_BOTTOM_PX) {
      sticking.current = true
      gliding.current = false
      return
    }

    // Every frame of a smooth scroll is a position short of the bottom, and
    // reading those as "the user left" would release autoscroll on the very
    // animation that is carrying them there.
    if (!gliding.current) sticking.current = false
  }, [])

  const stick = React.useCallback(() => {
    sticking.current = true
  }, [])

  return { ref, onScroll, stick }
}
