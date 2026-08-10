"use client"

import type { ReactNode } from "react"

import { FaberLogo } from "@/components/ui/logos"
import { AuthGate, SurgeAuthProvider } from "@/components/ui/surge-auth"

/**
 * Origin serving the Surge `/v1` perimeter. Defaults to this app's own origin,
 * which is the deployment shape where the API mounts `browser_router()` itself
 * — set `NEXT_PUBLIC_SURGE_URL` when Surge lives somewhere else.
 */
const BASE_URL = process.env.NEXT_PUBLIC_SURGE_URL ?? "/"

/**
 * Everything below this renders only for a signed-in identity; the sign-in
 * flow runs inline, over the app's own ambient backdrop.
 */
export function AppAuth({ children }: { children: ReactNode }) {
  return (
    <SurgeAuthProvider
      baseUrl={BASE_URL}
      mode="inline"
      mark={<FaberLogo size={44} aria-hidden />}
    >
      <AuthGate>{children}</AuthGate>
    </SurgeAuthProvider>
  )
}
