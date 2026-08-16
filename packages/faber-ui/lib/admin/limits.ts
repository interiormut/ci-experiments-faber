/**
 * Capacity arithmetic that only an operator asks for.
 *
 * The wording of a single limit lives in `lib/hosts/limits.ts`, since a tenant
 * reading their own share and an operator reading everyone's should see the
 * same number said the same way. What is here is the part that has no meaning
 * to a tenant: how much of a machine is already promised, and how one person's
 * usage sits against their grant.
 */

import type { ServiceHostStorage, Tenant } from "@/lib/api"

/**
 * How much of a host's promisable storage is already promised.
 *
 * Against the ceiling rather than against the disk: the ceiling is capacity
 * less the reserve faber keeps free, and it is the number a grant is actually
 * checked against. Showing the raw disk would suggest room that no grant can
 * ever have.
 */
export function committedFraction(storage: ServiceHostStorage): number {
  if (storage.ceiling_bytes <= 0) return 0
  return Math.min(1, storage.committed_bytes / storage.ceiling_bytes)
}

/**
 * What a tenant is using against what they are allowed, or `null` when either
 * half is missing.
 *
 * Usage comes from the machine on every request and can be absent — a
 * controller not enabled, a project quota not readable — and an unlimited
 * grant has no bar to fill. Both cases are "no bar" rather than a bar drawn
 * from a guess.
 */
export function usedFraction(tenant: Tenant): number | null {
  const used = tenant.usage.storage_bytes
  const limit = tenant.quota.storage_bytes
  if (used === null || limit === null || limit <= 0) return null
  return Math.min(1, used / limit)
}
