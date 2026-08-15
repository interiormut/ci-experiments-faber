export type FaberRuntimeConfig = {
  apiUrl: string
  authMode?: "inline" | "redirect"
}

declare global {
  interface Window {
    __FABER_RUNTIME_CONFIG__?: FaberRuntimeConfig
  }
}

/**
 * Configuration injected by the static asset server at request time.
 *
 * In development, Vite's proxy makes the API same-origin, so an empty
 * `apiUrl` is correct and no build-time value is needed.
 */
export function getRuntimeConfig(): FaberRuntimeConfig {
  return window.__FABER_RUNTIME_CONFIG__ ?? { apiUrl: "" }
}

export function getRuntimeApiUrl(): string {
  return getRuntimeConfig().apiUrl
}
