// Deadline handling shared by the upstream clients (steamtools, DepotBox, Ryu, GitHub). Each client
// passes its own error type, so routes keep mapping failures by `instanceof` — including a deadline
// that expires while a body is still being read, which surfaces as an abort rather than a client error.

import { PassThrough, Readable } from "node:stream";

export interface Deadline {
  /** How the upstream is named in error messages, e.g. "DepotBox". */
  label: string;
  /** Builds the client's own error type from an HTTP status, a message and, for a 429, the wait. */
  error: (status: number, message: string, retryAfterSeconds?: number) => Error;
}

// Headers that carry an upstream credential. They are only ever sent to the origin they belong to.
const CREDENTIAL_HEADERS = new Set(["x-api-key", "authorization"]);
const MAXIMUM_REDIRECTS = 5;

// A timeout maps to 504, anything else to 502 — the same whether it happens before or after the headers.
function failure(timedOut: boolean, { label, error }: Deadline): Error {
  return timedOut ? error(504, `${label} request timed out.`) : error(502, `${label} request failed.`);
}

function isAbort(error: unknown): boolean {
  return error instanceof Error && (error.name === "AbortError" || error.name === "TimeoutError");
}

// Seconds from `Retry-After`, or from `X-RateLimit-Reset` as SteamTools sends it.
function retryAfterSeconds(headers: Headers): number | undefined {
  for (const name of ["retry-after", "x-ratelimit-reset"]) {
    const seconds = Number.parseInt(headers.get(name) ?? "", 10);
    if (Number.isFinite(seconds) && seconds >= 0) return seconds;
  }
  return undefined;
}

function withoutCredentials(headers: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(headers).filter(([name]) => !CREDENTIAL_HEADERS.has(name.toLowerCase())));
}

// Fetches `url` under a deadline of `timeoutMs`. The deadline normally also covers reading the body; a
// `streamed` body is piped to the client and read at the client's pace, so its deadline ends once the
// headers have arrived.
//
// Redirects are followed here rather than by fetch, which would forward an `x-api-key` header to
// whatever host a provider redirects to. A 429 becomes the client's error with the requested wait.
export async function fetchWithDeadline(
  url: string,
  headers: Record<string, string>,
  timeoutMs: number,
  deadline: Deadline,
  streamed = false,
): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  timer.unref();
  try {
    let target = new URL(url);
    let sent = headers;
    for (let redirects = 0; ; redirects += 1) {
      let response: Response;
      try {
        response = await fetch(target, { headers: sent, signal: controller.signal, redirect: "manual" });
      } catch {
        throw failure(controller.signal.aborted, deadline);
      }
      const location = response.headers.get("location");
      if (response.status >= 300 && response.status < 400 && location) {
        await response.body?.cancel();
        if (redirects >= MAXIMUM_REDIRECTS) throw deadline.error(502, `${deadline.label} redirected too often.`);
        const next = new URL(location, target);
        if (next.origin !== target.origin) sent = withoutCredentials(sent);
        target = next;
        continue;
      }
      if (response.status === 429) {
        await response.body?.cancel();
        throw deadline.error(429, `${deadline.label} rate limit reached.`, retryAfterSeconds(response.headers));
      }
      return response;
    }
  } finally {
    if (streamed) clearTimeout(timer);
  }
}

// Awaits a buffered body read (`response.text()`, `response.json()`) with its failures mapped.
export async function readBody<T>(read: Promise<T>, deadline: Deadline): Promise<T> {
  try {
    return await read;
  } catch (error) {
    throw failure(isAbort(error), deadline);
  }
}

// The response body as a Node stream whose failures are mapped. A consumer that stops reading early
// (a client disconnecting, a failed cache write) cancels the upstream body.
export function streamBody(body: ReadableStream<Uint8Array>, deadline: Deadline): Readable {
  const source = Readable.fromWeb(body as Parameters<typeof Readable.fromWeb>[0]);
  const output = new PassThrough();
  source.on("error", (error) => output.destroy(failure(isAbort(error), deadline)));
  output.on("close", () => source.destroy());
  return source.pipe(output);
}
