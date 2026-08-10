"use client"

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react"
import { SurgeClient, type FlowResult, type Session } from "@panit/surge-client"

import { AuthDialog, type AuthDialogView } from "@/components/ui/auth-dialog"

// ─── Types ───────────────────────────────────────────────────────────────────

/**
 * How the app sends users into authentication.
 *
 * - `inline`: the flow runs in `AuthDialog`, in place, against the `/v1`
 *   perimeter of whatever origin the client points at. Needs `allow_inline`
 *   on the server.
 * - `redirect`: the browser navigates to `GET /v1/login`, which hands off to
 *   the deployment's hosted auth UI and comes back to `return_to`.
 */
export type SurgeAuthMode = "inline" | "redirect"

/**
 * `offline` is deliberately distinct from `unauthed`: only a 401 proves the
 * session is gone. A transport failure, a 5xx from the perimeter, or an
 * upstream Surge outage behind a proxying deployment are all "we don't know",
 * and tearing the session down over one signs users out over a blip.
 */
export type SurgeAuthStatus = "loading" | "authed" | "unauthed" | "offline"

/** What `AuthGate` renders, independent of where that answer came from. */
export type AuthGateStatus = "loading" | "authed" | "unauthed"

type SurgeAuthState = {
  status: SurgeAuthStatus
  /** Last known session. Survives an `offline` refresh, cleared on `unauthed`. */
  session: Session | null
}

type SurgeAuthContextValue = SurgeAuthState & {
  client: SurgeClient
  mode: SurgeAuthMode
  /** EFF wordlist for passphrase login/recovery. Absent hides passphrase options. */
  wordlist?: string[]
  /** Brand mark rendered above auth headings. */
  mark?: ReactNode
  /** Re-reads `GET /v1/whoami` and updates `status`/`session`. */
  refresh: () => Promise<SurgeAuthStatus>
  /**
   * Starts a sign-in. In `redirect` mode this navigates away and never
   * returns; in `inline` mode it is a no-op — render `Reauth` or `AuthGate`
   * instead, which own the dialog.
   */
  login: (returnTo?: string) => void
  /** Revokes the session server-side, then clears local auth state. */
  logout: () => Promise<void>
}

// ─── Provider ────────────────────────────────────────────────────────────────

const SurgeAuthContext = createContext<SurgeAuthContextValue | null>(null)

export type SurgeAuthProviderProps = {
  children: ReactNode
  /**
   * A pre-built client. Mutually exclusive with `baseUrl` — pass this when the
   * app already owns a client (e.g. a singleton shared with non-React code).
   */
  client?: SurgeClient
  /** Origin serving the Surge `/v1` perimeter. Ignored when `client` is given. */
  baseUrl?: string
  mode?: SurgeAuthMode
  wordlist?: string[]
  mark?: ReactNode
  /**
   * Check the session on mount (default `true`). Turn it off when an outer
   * store already establishes the session, or in stories and tests that must
   * stay off the network — `status` then starts at `unauthed` unless
   * `initialSession` says otherwise.
   */
  autoRefresh?: boolean
  initialSession?: Session | null
}

/**
 * Session state for a Surge `/v1` perimeter, shared by every auth surface in
 * the app. Knows nothing about the host application: the only contract is the
 * perimeter itself, so the same provider works against a standalone
 * surge-server or an API that mounts `browser_router()` on its own origin.
 */
export function SurgeAuthProvider({
  children,
  client: providedClient,
  baseUrl,
  mode = "inline",
  wordlist,
  mark,
  autoRefresh = true,
  initialSession = null,
}: SurgeAuthProviderProps) {
  const client = useMemo(() => {
    if (providedClient) return providedClient
    if (!baseUrl) {
      throw new Error("SurgeAuthProvider needs either `client` or `baseUrl`")
    }
    return new SurgeClient({ baseUrl })
  }, [providedClient, baseUrl])

  const [state, setState] = useState<SurgeAuthState>({
    status: autoRefresh ? "loading" : initialSession ? "authed" : "unauthed",
    session: initialSession,
  })
  const mountedRef = useRef(true)

  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  const refresh = useCallback(async (): Promise<SurgeAuthStatus> => {
    let next: SurgeAuthState
    try {
      const session = await client.whoami()
      next = session
        ? { status: "authed", session }
        : { status: "unauthed", session: null }
    } catch {
      next = { status: "offline", session: null }
    }

    if (!mountedRef.current) return next.status

    setState((prev) => ({
      status: next.status,
      // An unreachable perimeter says nothing about the session, so the last
      // known one is kept for callers that render optimistically while offline.
      session: next.status === "offline" ? prev.session : next.session,
    }))

    return next.status
  }, [client])

  useEffect(() => {
    if (!autoRefresh) return
    void refresh()
  }, [autoRefresh, refresh])

  const login = useCallback(
    (returnTo?: string) => {
      if (mode !== "redirect") return
      const target =
        returnTo ??
        (typeof window !== "undefined" ? window.location.href : undefined)
      if (!target) return
      window.location.assign(client.loginUrl(target))
    },
    [client, mode],
  )

  const logout = useCallback(async () => {
    try {
      await client.logout()
    } catch {
      // Logout is best-effort: a failed revoke must still drop local state.
    }
    if (!mountedRef.current) return
    setState({ status: "unauthed", session: null })
  }, [client])

  const value = useMemo<SurgeAuthContextValue>(
    () => ({ ...state, client, mode, wordlist, mark, refresh, login, logout }),
    [state, client, mode, wordlist, mark, refresh, login, logout],
  )

  return (
    <SurgeAuthContext.Provider value={value}>
      {children}
    </SurgeAuthContext.Provider>
  )
}

export function useSurgeAuth(): SurgeAuthContextValue {
  const ctx = useContext(SurgeAuthContext)
  if (!ctx) throw new Error("useSurgeAuth must be used within SurgeAuthProvider")
  return ctx
}

export function useOptionalSurgeAuth(): SurgeAuthContextValue | null {
  return useContext(SurgeAuthContext)
}

// ─── Session watch ───────────────────────────────────────────────────────────

/**
 * Detects a session that expired while the app sat open.
 *
 * Active use needs no watcher: any API call that 401s can trip the app's own
 * unauthorized handling. The gap this closes is the idle tab — left open for
 * hours with nothing in flight — so it checks on focus and visibility rather
 * than on a timer. That catches the realistic case sooner than a poll would,
 * and costs nothing while the tab sits idle.
 *
 * It reads `GET /v1/whoami` directly instead of going through the provider's
 * `refresh`, so a mid-session expiry does not flip global auth state and
 * unmount the app underneath the re-auth overlay.
 *
 * @param enabled Watch only while there is a session to lose — pass `false`
 *   when signed out. There is no check on mount: whoever established the
 *   session just called whoami, and repeating it doubles every page load.
 */
export function useSessionWatch(enabled: boolean) {
  const { client } = useSurgeAuth()
  const [expired, setExpired] = useState(false)

  const markResolved = useCallback(() => setExpired(false), [])

  useEffect(() => {
    if (!enabled) {
      setExpired(false)
      return
    }

    let cancelled = false

    const check = () => {
      if (document.visibilityState !== "visible") return

      void client
        .whoami()
        .then((session) => {
          if (cancelled) return
          setExpired(session === null)
        })
        // A failed probe is not evidence of being signed out — the perimeter
        // may just be unreachable, and the session may still be valid.
        .catch(() => {})
    }

    window.addEventListener("focus", check)
    document.addEventListener("visibilitychange", check)

    return () => {
      cancelled = true
      window.removeEventListener("focus", check)
      document.removeEventListener("visibilitychange", check)
    }
  }, [enabled, client])

  return { expired, markResolved }
}

// ─── Reauth ──────────────────────────────────────────────────────────────────

export type ReauthProps = {
  open: boolean
  /** Fired after a successful sign-in, once auth state has been refreshed. */
  onAuthenticated?: (result: FlowResult) => void
  /** Overrides for the values `SurgeAuthProvider` supplies. */
  client?: SurgeClient
  mode?: SurgeAuthMode
  mark?: ReactNode
  wordlist?: string[]
  defaultView?: AuthDialogView
  /** Allow dismissing without authenticating (default: false). */
  dismissible?: boolean
}

/**
 * The single sign-in surface, for both "never signed in" and "session
 * expired". Whether the app stays mounted behind the overlay is the caller's
 * call — see `AuthGate`.
 *
 * Stays mounted across `open` changes rather than being conditionally
 * rendered: the dialog's close animation (`data-[state=closed]:animate-out`)
 * only plays if the element survives long enough to run it. Unmounting on
 * success makes it vanish on the frame the flow completes.
 *
 * In redirect mode it renders nothing and sends the browser to Surge instead.
 */
export function Reauth({
  open,
  onAuthenticated,
  client,
  mode,
  mark,
  wordlist,
  defaultView,
  dismissible,
}: ReauthProps) {
  const {
    client: contextClient,
    mode: contextMode,
    mark: contextMark,
    wordlist: contextWordlist,
    login,
    refresh,
  } = useSurgeAuth()

  const resolvedClient = client ?? contextClient
  const resolvedMode = mode ?? contextMode
  const resolvedMark = mark ?? contextMark
  const resolvedWordlist = wordlist ?? contextWordlist

  // Gated on `open` precisely because this component is always mounted — a
  // bare mount effect here would redirect on every page load.
  useEffect(() => {
    if (open && resolvedMode === "redirect") login()
  }, [open, resolvedMode, login])

  if (resolvedMode === "redirect") return null

  const handleAuthenticated = (result: FlowResult) => {
    void refresh()
    onAuthenticated?.(result)
  }

  return (
    <AuthDialog
      surgeClient={resolvedClient}
      open={open}
      onAuthenticated={handleAuthenticated}
      mark={resolvedMark}
      wordlist={resolvedWordlist}
      defaultView={defaultView}
      dismissible={dismissible}
    />
  )
}

// ─── AuthGate ────────────────────────────────────────────────────────────────

export type AuthGateProps = {
  children: ReactNode
  /**
   * Overrides the status derived from `SurgeAuthProvider`. Pass this when the
   * app has a richer notion of "signed in" than the Surge session alone —
   * e.g. a profile store that must finish hydrating before the app mounts.
   */
  status?: AuthGateStatus
  /** Rendered while the status is `loading`. */
  fallback?: ReactNode
  /** Fired after a successful sign-in through the gate's dialog. */
  onAuthenticated?: (result: FlowResult) => void
  /** Overrides for the values `SurgeAuthProvider` supplies. */
  client?: SurgeClient
  mode?: SurgeAuthMode
  mark?: ReactNode
  wordlist?: string[]
}

/**
 * Sole owner of the three auth states: fallback while resolving, sign-in when
 * signed out, app otherwise — plus an overlay when a live session expires.
 *
 * Keeping all of them in one branch chain is deliberate. Split across sibling
 * components, a full-screen sign-in and a mid-session dialog have to stay
 * complements of each other by convention; once both render the same dialog,
 * they open together and stack two modals.
 *
 * Signed out, the app never mounts — there is nothing in flight to protect.
 * Expired mid-use, it stays mounted behind the overlay so in-progress work,
 * scroll position, and open views survive re-authentication.
 */
export function AuthGate({
  children,
  status,
  fallback = null,
  onAuthenticated,
  client,
  mode,
  mark,
  wordlist,
}: AuthGateProps) {
  const auth = useSurgeAuth()
  const resolved = status ?? deriveGateStatus(auth.status, auth.session !== null)
  const { expired, markResolved } = useSessionWatch(resolved === "authed")

  const content =
    resolved === "loading" ? fallback : resolved === "authed" ? children : null

  // `Reauth` sits outside that chain, mounted at all times and driven by a
  // single `open` expression. Conditionally rendering it would tear it out on
  // the frame sign-in succeeds, skipping its close animation.
  return (
    <>
      {content}
      <Reauth
        open={resolved === "unauthed" || (resolved === "authed" && expired)}
        onAuthenticated={(result) => {
          markResolved()
          onAuthenticated?.(result)
        }}
        client={client}
        mode={mode}
        mark={mark}
        wordlist={wordlist}
      />
    </>
  )
}

/**
 * `offline` is not a gate state: an unreachable perimeter is not evidence of
 * being signed out, so a known session keeps the app up. With no session to
 * fall back on it resolves to the sign-in surface rather than a splash that
 * never ends — the flow's own error then tells the user what broke.
 */
function deriveGateStatus(
  status: SurgeAuthStatus,
  hasSession: boolean,
): AuthGateStatus {
  if (status === "authed") return "authed"
  if (status === "offline") return hasSession ? "authed" : "unauthed"
  if (status === "unauthed") return "unauthed"
  return "loading"
}
