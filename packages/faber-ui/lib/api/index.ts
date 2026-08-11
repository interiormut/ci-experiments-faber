/**
 * Client for the faber API (`crates/api`).
 *
 * Browser-side: requests carry the `surge_session` cookie and relative URLs
 * resolve against the page origin, so call these from client components (or
 * from a route handler that forwards the cookie itself).
 *
 * ```ts
 * import { faber, FaberError } from "@/lib/api"
 *
 * try {
 *   const { root_thread } = await faber.createSession({ title: "First run" })
 * } catch (error) {
 *   if (error instanceof FaberError && error.isUnauthorized) signIn()
 * }
 * ```
 */

import { FaberClient } from "./client"

export { FaberClient, type FaberClientOptions } from "./client"
export { FaberError, errorFromResponse } from "./errors"
export type * from "./types"

/**
 * Origin the API is reachable at. Defaults to this app's own origin, which is
 * the deployment shape where the API serves the frontend — set
 * `NEXT_PUBLIC_FABER_API_URL` when the two are split (as in local dev, where
 * the API listens on its own port).
 */
const BASE_URL = process.env.NEXT_PUBLIC_FABER_API_URL ?? ""

/** Shared client. Construct a `FaberClient` directly to point somewhere else. */
export const faber = new FaberClient({ baseUrl: BASE_URL })
