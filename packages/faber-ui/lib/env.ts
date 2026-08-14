import { getRuntimeConfig } from "./runtime-config"

export type AuthMode = "inline" | "redirect"

function readAuthMode(value: string | undefined): AuthMode {
  return value === "redirect" ? "redirect" : "inline"
}

export const AUTH_MODE = readAuthMode(
  getRuntimeConfig().authMode ?? process.env.NEXT_PUBLIC_FABER_AUTH_MODE,
)
