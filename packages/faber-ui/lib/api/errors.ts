/**
 * The API answers every failure with `{ "error": string }` and an HTTP status
 * (`crates/api/src/error.rs`). There is no machine-readable code on the wire,
 * so branching logic keys on {@link FaberError.status} — the message is a
 * human-facing string and may change.
 */
export class FaberError extends Error {
  /** HTTP status of the response, or 0 when the request never completed. */
  readonly status: number

  constructor(status: number, message?: string) {
    super(message ?? `request failed with status ${status}`)
    this.name = "FaberError"
    this.status = status
  }

  /** The session is missing or expired — re-run the Surge sign-in flow. */
  get isUnauthorized(): boolean {
    return this.status === 401
  }

  get isNotFound(): boolean {
    return this.status === 404
  }

  /** 503 (pool exhausted, Surge unreachable) and 502 are worth a retry with backoff. */
  get isRetryable(): boolean {
    return this.status === 502 || this.status === 503
  }
}

/** Builds a `FaberError` from a non-2xx response, tolerating non-JSON bodies. */
export async function errorFromResponse(response: Response): Promise<FaberError> {
  let body: unknown
  try {
    body = await response.json()
  } catch {
    return new FaberError(
      response.status,
      `the API returned ${response.status} with a non-JSON body`,
    )
  }

  if (typeof body === "object" && body !== null && "error" in body) {
    const { error } = body as { error?: unknown }
    if (typeof error === "string") return new FaberError(response.status, error)
  }

  return new FaberError(
    response.status,
    `the API returned ${response.status} with an unrecognized error body`,
  )
}
