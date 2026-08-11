import { FaberError, errorFromResponse } from "./errors"
import type {
  CreateCredentialRequest,
  CreateModelRequest,
  CreateSessionRequest,
  CreateThreadRequest,
  CreatedSession,
  Credential,
  ListSessionsQuery,
  Me,
  ModelConfig,
  Run,
  SendMessageRequest,
  SendMessageResponse,
  Session,
  SpineEntry,
  StreamEvent,
  StreamQuery,
  Thread,
  TranscriptEvent,
  TranscriptQuery,
  UpdateModelRequest,
  UpdateSessionRequest,
  Uuid,
  Workspace,
} from "./types"

export interface FaberClientOptions {
  /**
   * Origin the API is reachable at, e.g. `https://faber.example.com`. Empty
   * (the default) means this app's own origin, which is the deployment shape
   * where the frontend is served by the API — set `NEXT_PUBLIC_FABER_API_URL`
   * when the two are split. A trailing slash is tolerated.
   */
  baseUrl?: string
  /** Custom `fetch`, for tests or non-browser runtimes. Defaults to the global. */
  fetch?: typeof fetch
}

type QueryValue = string | number | boolean | undefined

/**
 * Browser client for the faber API.
 *
 * Every request is sent with `credentials: "include"` so the browser attaches
 * the `surge_session` cookie the auth flow set — the API authenticates from
 * that cookie (falling back to a `Bearer` token) and provisions the local user
 * row on first call. A request made before sign-in fails with a
 * {@link FaberError} carrying status 401.
 *
 * Cross-origin use additionally requires the frontend's origin in the server's
 * `CORS_ORIGINS`.
 */
export class FaberClient {
  readonly baseUrl: string
  private readonly fetch: typeof fetch

  constructor(options: FaberClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? "").replace(/\/+$/, "")
    this.fetch = options.fetch ?? globalThis.fetch.bind(globalThis)
  }

  // -------------------------------------------------------------------------
  // Identity
  // -------------------------------------------------------------------------

  /** Liveness probe. The only route that does not require a session. */
  async health(): Promise<{ ok: boolean }> {
    return this.request("GET", "/health")
  }

  /**
   * The caller's local faber user row, provisioned on first call. Only carries
   * the local user id — fetch username/display_name/avatar_url from Surge's
   * own whoami instead.
   */
  async me(): Promise<Me> {
    return this.request("GET", "/api/me")
  }

  /**
   * Revokes the Surge session and clears the cookie. Succeeds even when the
   * caller is already signed out — logout never itself requires a session.
   */
  async logout(): Promise<void> {
    await this.request("POST", "/api/logout")
  }

  // -------------------------------------------------------------------------
  // Credentials
  // -------------------------------------------------------------------------

  async listCredentials(): Promise<Credential[]> {
    return this.request("GET", "/api/credentials")
  }

  /** The key is encrypted at rest and never readable again through the API. */
  async createCredential(body: CreateCredentialRequest): Promise<Credential> {
    return this.request("POST", "/api/credentials", { body })
  }

  async deleteCredential(id: Uuid): Promise<void> {
    await this.request("DELETE", `/api/credentials/${encodeURIComponent(id)}`)
  }

  // -------------------------------------------------------------------------
  // Models
  // -------------------------------------------------------------------------

  async listModels(): Promise<ModelConfig[]> {
    return this.request("GET", "/api/models")
  }

  async createModel(body: CreateModelRequest): Promise<ModelConfig> {
    return this.request("POST", "/api/models", { body })
  }

  async updateModel(id: Uuid, patch: UpdateModelRequest): Promise<ModelConfig> {
    return this.request("PATCH", `/api/models/${encodeURIComponent(id)}`, {
      body: patch,
    })
  }

  async deleteModel(id: Uuid): Promise<void> {
    await this.request("DELETE", `/api/models/${encodeURIComponent(id)}`)
  }

  // -------------------------------------------------------------------------
  // Workspaces
  // -------------------------------------------------------------------------

  /** Every workspace the caller belongs to; the personal one always appears. */
  async listWorkspaces(): Promise<Workspace[]> {
    return this.request("GET", "/api/workspaces")
  }

  // -------------------------------------------------------------------------
  // Sessions
  // -------------------------------------------------------------------------

  /** Newest first, across every workspace unless narrowed to one. */
  async listSessions(query: ListSessionsQuery = {}): Promise<Session[]> {
    return this.request("GET", "/api/sessions", { query })
  }

  /**
   * Creates a session and its root thread atomically. With no arguments the
   * session lands in the caller's personal workspace, untitled.
   */
  async createSession(body: CreateSessionRequest = {}): Promise<CreatedSession> {
    return this.request("POST", "/api/sessions", { body })
  }

  async getSession(id: Uuid): Promise<Session> {
    return this.request("GET", `/api/sessions/${encodeURIComponent(id)}`)
  }

  async updateSession(id: Uuid, patch: UpdateSessionRequest): Promise<Session> {
    return this.request("PATCH", `/api/sessions/${encodeURIComponent(id)}`, {
      body: patch,
    })
  }

  /**
   * Deletes the session and, by cascade, every thread, run, exchange, and
   * transcript row under it — including history that is otherwise append-only.
   * `updateSession(id, { closed: true })` is the reversible alternative.
   */
  async deleteSession(id: Uuid): Promise<void> {
    await this.request("DELETE", `/api/sessions/${encodeURIComponent(id)}`)
  }

  // -------------------------------------------------------------------------
  // Threads
  // -------------------------------------------------------------------------

  /** Every thread in the session, oldest first — roots and forks alike. */
  async listThreads(sessionId: Uuid): Promise<Thread[]> {
    return this.request(
      "GET",
      `/api/sessions/${encodeURIComponent(sessionId)}/threads`,
    )
  }

  /**
   * Adds a thread to the session: a second root by default, or a fork when
   * `parent_id` and `forked_at_seq` are both given.
   */
  async createThread(
    sessionId: Uuid,
    body: CreateThreadRequest = {},
  ): Promise<Thread> {
    return this.request(
      "POST",
      `/api/sessions/${encodeURIComponent(sessionId)}/threads`,
      { body },
    )
  }

  // -------------------------------------------------------------------------
  // Messages and streaming
  // -------------------------------------------------------------------------

  /**
   * Starts a harness run and returns as soon as it is registered — the answer
   * arrives over {@link streamSession}, not here.
   *
   * The run is detached server-side: it survives this response and every
   * subscriber disconnecting, so a dropped stream loses the live view, not the
   * work.
   */
  async sendMessage(
    sessionId: Uuid,
    body: SendMessageRequest,
  ): Promise<SendMessageResponse> {
    return this.request(
      "POST",
      `/api/sessions/${encodeURIComponent(sessionId)}/messages`,
      { body },
    )
  }

  /**
   * Everything happening in the session, as it happens.
   *
   * ```ts
   * const controller = new AbortController()
   * for await (const event of faber.streamSession(id, {}, controller.signal)) {
   *   if (event.kind === "run_end") break
   * }
   * ```
   *
   * Pass `{ run_id, after_seq }` to resume: the server replays that run's
   * events past the cursor before switching to live, so reconnecting leaves no
   * gap. Both fields or neither.
   *
   * Uses `fetch` rather than `EventSource` — `EventSource` cannot set headers
   * and its cross-origin credential story is worse than the `credentials:
   * "include"` this client already depends on everywhere else.
   *
   * A `lagged` frame means the subscription fell too far behind and events
   * were dropped; refetch through {@link listTranscript} and resume. It is
   * surfaced as a thrown {@link FaberError} rather than swallowed, because
   * silently continuing would leave a hole in the conversation.
   */
  async *streamSession(
    sessionId: Uuid,
    query: StreamQuery = {},
    signal?: AbortSignal,
  ): AsyncGenerator<StreamEvent> {
    const url = `${this.baseUrl}/api/sessions/${encodeURIComponent(sessionId)}/stream${queryString(query)}`

    const response = await this.fetch(url, {
      method: "GET",
      credentials: "include",
      signal,
      headers: { Accept: "text/event-stream" },
    })

    if (!response.ok) throw await errorFromResponse(response)
    if (!response.body) throw new FaberError(0, "the stream response had no body")

    const reader = response.body
      .pipeThrough(new TextDecoderStream())
      .getReader()

    let buffer = ""
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (done) break
        buffer += value

        for (;;) {
          // SSE frames are separated by a blank line; `\r\n` is legal too.
          const boundary = /\r?\n\r?\n/.exec(buffer)
          if (!boundary) break
          const frame = buffer.slice(0, boundary.index)
          buffer = buffer.slice(boundary.index + boundary[0].length)

          const parsed = parseFrame(frame)
          if (!parsed) continue
          if (parsed.event === "lagged") {
            throw new FaberError(
              0,
              `the stream fell behind by ${parsed.data} events; refetch the transcript and resume`,
            )
          }
          if (parsed.event === "transcript") {
            yield JSON.parse(parsed.data) as StreamEvent
          }
        }
      }
    } finally {
      // Releasing the lock lets an abort actually tear the connection down
      // rather than leaving it half-open until GC.
      reader.releaseLock()
    }
  }

  async getThread(id: Uuid): Promise<Thread> {
    return this.request("GET", `/api/threads/${encodeURIComponent(id)}`)
  }

  /**
   * The thread's canonical history chain, in `seq` order — one position per
   * committed run, which is what the next turn's history is rebuilt from.
   */
  async listSpine(threadId: Uuid): Promise<SpineEntry[]> {
    return this.request(
      "GET",
      `/api/threads/${encodeURIComponent(threadId)}/spine`,
    )
  }

  async listRuns(threadId: Uuid): Promise<Run[]> {
    return this.request(
      "GET",
      `/api/threads/${encodeURIComponent(threadId)}/runs`,
    )
  }

  // -------------------------------------------------------------------------
  // Runs
  // -------------------------------------------------------------------------

  /**
   * The run's event stream in `seq` order — the durable record behind
   * {@link streamSession}. Pass the last `seq` you saw as `after_seq` to fetch
   * the tail, which is also how you re-sync after a `lagged` frame.
   */
  async listTranscript(
    runId: Uuid,
    query: TranscriptQuery = {},
  ): Promise<TranscriptEvent[]> {
    return this.request(
      "GET",
      `/api/runs/${encodeURIComponent(runId)}/transcript`,
      { query },
    )
  }

  // -------------------------------------------------------------------------
  // Transport
  // -------------------------------------------------------------------------

  private async request<T>(
    method: string,
    path: string,
    options: {
      body?: unknown
      query?: Record<string, QueryValue>
      signal?: AbortSignal
    } = {},
  ): Promise<T> {
    const url = `${this.baseUrl}${path}${queryString(options.query)}`

    const response = await this.fetch(url, {
      method,
      credentials: "include",
      signal: options.signal,
      headers:
        options.body === undefined
          ? { Accept: "application/json" }
          : { Accept: "application/json", "Content-Type": "application/json" },
      // `undefined` members drop out here, which is what makes an omitted
      // patch field different from an explicit `null` on the wire.
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    })

    if (!response.ok) throw await errorFromResponse(response)

    // 204 on every delete and on logout — nothing to parse.
    if (response.status === 204) return undefined as T

    return (await response.json()) as T
  }
}

/**
 * Splits one SSE frame into its `event:` name and joined `data:` payload.
 *
 * Returns `null` for a frame carrying no data — a comment-only keep-alive
 * (`: ping`) is exactly that, and arrives on every idle interval.
 */
function parseFrame(frame: string): { event: string; data: string } | null {
  let event = "message"
  const data: string[] = []

  for (const line of frame.split(/\r?\n/)) {
    if (line.startsWith(":")) continue
    const colon = line.indexOf(":")
    const field = colon === -1 ? line : line.slice(0, colon)
    // One optional space after the colon is part of the framing, not the value.
    const value = colon === -1 ? "" : line.slice(colon + 1).replace(/^ /, "")

    if (field === "event") event = value
    else if (field === "data") data.push(value)
  }

  return data.length === 0 ? null : { event, data: data.join("\n") }
}

function queryString(query?: Record<string, QueryValue>): string {
  if (!query) return ""
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined) params.set(key, String(value))
  }
  const encoded = params.toString()
  return encoded ? `?${encoded}` : ""
}
