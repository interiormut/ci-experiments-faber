"use client"

import type { ReactNode } from "react"

import { FaberLogo } from "@/components/ui/logos"
import { AuthGate, SurgeAuthProvider } from "@/components/ui/surge-auth"

/**
 * The Surge `/v1` perimeter, which the API always mounts under `/api/surge` on
 * its own origin — so it follows wherever the API lives. Defaults to this app's
 * own origin, which is the deployment shape where the API serves the frontend.
 */
const API_URL = (process.env.NEXT_PUBLIC_FABER_API_URL ?? "").replace(/\/+$/, "")
const BASE_URL = `${API_URL}/api/surge`

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
