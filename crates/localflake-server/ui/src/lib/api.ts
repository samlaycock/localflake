/** An error response from localflake's UI API, or no response at all (status 0). */
export class ApiError extends Error {
  status: number
  data: unknown
  /** Snowflake's error code, such as 390111 for a session that has ended. */
  code: string | undefined

  constructor(message: string, status: number, data: unknown) {
    super(message)
    this.name = "ApiError"
    this.status = status
    this.data = data
    this.code = errorOf(data)?.code
  }
}

function errorOf(data: unknown): { code?: string; message?: string } | null {
  if (data && typeof data === "object" && "error" in data) {
    const error = (data as { error: unknown }).error
    if (error && typeof error === "object") return error
  }
  return null
}

/** Calls localflake's UI API (`/__localflake/api`). */
export async function api<T>(
  path: string,
  {
    method = "GET",
    body,
    raw,
    signal,
  }: { method?: string; body?: unknown; raw?: Blob; signal?: AbortSignal } = {}
): Promise<T> {
  const headers = new Headers()
  let payload: BodyInit | undefined
  if (raw !== undefined) {
    payload = raw
    headers.set("Content-Type", "application/octet-stream")
  } else if (body !== undefined) {
    payload = JSON.stringify(body)
    headers.set("Content-Type", "application/json")
  }
  let response: Response
  try {
    response = await fetch(`/__localflake/api${path}`, {
      method,
      headers,
      body: payload,
      signal,
    })
  } catch (e) {
    if (signal?.aborted) throw e
    throw new ApiError(
      "localflake isn't responding. Is it still running?",
      0,
      null
    )
  }
  let data: unknown = {}
  try {
    data = await response.json()
  } catch {
    // An empty or non-JSON body.
  }
  if (!response.ok) {
    const message =
      errorOf(data)?.message ?? `${response.status} ${response.statusText}`
    throw new ApiError(message, response.status, data)
  }
  return data as T
}

export const message = (e: unknown) =>
  e instanceof Error ? e.message : String(e)
