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
 * view jumps once per row, rather than riding every token that fills one in.
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
 * re-arms.
 *
 * Keyboard scrolling is not handled, because the container is not focusable —
 * arrow and page keys never reach it, focus being on the prompt box or the
 * body. Scrolling by dragging the scrollbar is covered by the position rule
 * rather than by an intent listener of its own.
 */
export function useStickToBottom<T extends HTMLElement>(content: unknown): StickToBottom<T> {
  const ref = React.useRef<T>(null)
  const sticking = React.useRef(true)

  React.useEffect(() => {
    const el = ref.current
    if (!el || !sticking.current) return
    el.scrollTop = el.scrollHeight
  }, [content])

  React.useEffect(() => {
    const el = ref.current
    if (!el) return

    const release = () => {
      sticking.current = false
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
    sticking.current = el.scrollHeight - el.scrollTop - el.clientHeight < AT_BOTTOM_PX
  }, [])

  const stick = React.useCallback(() => {
    sticking.current = true
  }, [])

  return { ref, onScroll, stick }
}
