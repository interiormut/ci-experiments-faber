/**
 * Rendering and reading the four per-user ceilings on a shared machine.
 *
 * One rule runs through all of it: **`null` is unlimited, never "unset" and
 * never "inherit"**. A grant replaces a host's defaults as a whole row, so an
 * empty field means unlimited rather than "leave what was there", and a form
 * built on these has to say so out loud — somebody who clears a box and
 * expects the default back would otherwise grant the opposite of what they
 * meant.
 *
 * Both the tenant's view of their own share and the operator's view of
 * everyone's read from here, so the same number is worded the same way on both
 * sides of that fence.
 */

import type { Limits } from "@/lib/api"

const GIB = 1024 * 1024 * 1024

/** Bytes as a short size. Unlimited is a word, not a blank. */
export function bytes(value: number | null | undefined): string {
  if (value === null || value === undefined) return "unlimited"
  if (value >= GIB) return `${round(value / GIB)} GiB`
  if (value >= 1024 * 1024) return `${round(value / (1024 * 1024))} MiB`
  return `${value} B`
}

function round(value: number): string {
  return value >= 10 ? String(Math.round(value)) : value.toFixed(1).replace(/\.0$/, "")
}

/** CPU as cores, since a thousandth of a core is not a unit anyone thinks in. */
export function cores(millis: number | null): string {
  if (millis === null) return "unlimited"
  return `${round(millis / 1000)} ${millis === 1000 ? "core" : "cores"}`
}

export function count(value: number | null): string {
  return value === null ? "unlimited" : String(value)
}

/** The four limits as one line, for a row that has no room for a table. */
export function summary(limits: Limits): string {
  return [
    cores(limits.cpu_millis),
    `${bytes(limits.memory_bytes)} RAM`,
    `${bytes(limits.storage_bytes)} disk`,
    `${count(limits.container_max)} containers`,
  ].join(" · ")
}

/**
 * A form value — GiB as typed — back to bytes, or `null` for unlimited.
 *
 * Blank is unlimited on purpose and the forms label it that way. A value that
 * does not parse is also `null`: the server refuses zero and negatives, and
 * inventing a number here would be worse than sending the honest "no ceiling"
 * the field is showing.
 */
export function gibToBytes(value: string): number | null {
  const trimmed = value.trim()
  if (!trimmed) return null
  const parsed = Number(trimmed)
  if (!Number.isFinite(parsed) || parsed <= 0) return null
  return Math.round(parsed * GIB)
}

/** Bytes back into a GiB form value; unlimited is an empty field. */
export function bytesToGib(value: number | null): string {
  if (value === null) return ""
  return String(Number((value / GIB).toFixed(2)))
}

export function millisToCores(value: number | null): string {
  if (value === null) return ""
  return String(Number((value / 1000).toFixed(2)))
}

export function coresToMillis(value: string): number | null {
  const trimmed = value.trim()
  if (!trimmed) return null
  const parsed = Number(trimmed)
  if (!Number.isFinite(parsed) || parsed <= 0) return null
  return Math.round(parsed * 1000)
}

export function toInteger(value: string): number | null {
  const trimmed = value.trim()
  if (!trimmed) return null
  const parsed = Number(trimmed)
  if (!Number.isInteger(parsed) || parsed <= 0) return null
  return parsed
}

