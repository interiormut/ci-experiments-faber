export type FaberRuntimeConfig = {
  apiUrl: string
  authMode?: "inline" | "redirect"
}

declare global {
  interface Window {
    __FABER_RUNTIME_CONFIG__?: FaberRuntimeConfig
  }
}

/** Reads configuration injected by the static asset server at request time. */
export function getRuntimeApiUrl(): string {
  return getRuntimeConfig().apiUrl
}

export function getRuntimeConfig(): FaberRuntimeConfig {
  if (typeof window !== "undefined") return window.__FABER_RUNTIME_CONFIG__ ?? { apiUrl: "" }
  return { apiUrl: "" }
}
