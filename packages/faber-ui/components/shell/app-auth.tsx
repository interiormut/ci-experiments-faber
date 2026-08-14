"use client"

import type { ReactNode } from "react"

import { FaberLogo } from "@/components/ui/logos"
import { AuthGate, SurgeAuthProvider } from "@/components/ui/surge-auth"
import { AUTH_MODE } from "@/lib/env"
import { getRuntimeApiUrl } from "@/lib/runtime-config"

/**
 * The Surge `/v1` perimeter, which the API always mounts under `/api/surge` on
 * its own origin — so it follows wherever the API lives. Defaults to this app's
 * own origin, which is the deployment shape where the API serves the frontend.
 */
const API_URL = getRuntimeApiUrl().replace(/\/+$/, "")
const BASE_URL = `${API_URL}/api/surge`

/**
 * Everything below this renders only for a signed-in identity; the sign-in
 * flow uses the configured inline or redirect mode.
 */
export function AppAuth({ children }: { children: ReactNode }) {
  return (
    <SurgeAuthProvider
      baseUrl={BASE_URL}
      mode={AUTH_MODE}
      mark={<FaberLogo size={44} aria-hidden />}
    >
      <AuthGate>{children}</AuthGate>
    </SurgeAuthProvider>
  )
}
