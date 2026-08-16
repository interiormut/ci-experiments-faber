"use client"

import * as React from "react"

import { faber, type HostUsage, type Uuid } from "@/lib/api"

/**
 * The caller's live footprint on a service host.
 *
 * Fetched separately from the host list, and deliberately not folded into it:
 * every call reads the machine — the cgroup's counters and the filesystem's
 * project quota — so bundling it into the list would read the machine once per
 * host before anything renders, on a page that mostly shows machines with no
 * quota at all.
 *
 * A failure is silent and leaves `usage` null. This is a supporting number
 * next to the grant, not the page: an error banner because a counter was
 * unreadable would be louder than what it says.
 */
export function useHostUsage(hostId: Uuid | null, enabled: boolean) {
  const [usage, setUsage] = React.useState<HostUsage | null>(null)

  React.useEffect(() => {
    if (!hostId || !enabled) return
    let cancelled = false

    void faber
      .hostUsage(hostId)
      .then((value) => {
        if (!cancelled) setUsage(value)
      })
      .catch(() => {})

    return () => {
      cancelled = true
    }
  }, [hostId, enabled])

  return usage
}
