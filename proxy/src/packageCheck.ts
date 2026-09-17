// Checks a depot package before it is cached. A provider can answer status 200 with an HTML error
// page, a Cloudflare challenge or a JSON error; cached, that would be handed to every client for a
// day while the next provider, which might have the real package, is never asked.

import { Readable } from "node:stream";
import type { RawStream } from "./depotbox.js";
import { UpstreamError } from "./upstream.js";

const ZIP_SIGNATURE = Buffer.from([0x50, 0x4b, 0x03, 0x04]);

// The same ceiling the Drydock client applies to a depot package.
export const MAXIMUM_PACKAGE_BYTES = 256 * 1024 * 1024;

// Returns the package body once its content type and first bytes show a ZIP archive. The returned
// stream still yields the whole body, and fails if it grows past `maximumBytes`.
export async function requireZip(
  raw: RawStream,
  label: string,
  maximumBytes = MAXIMUM_PACKAGE_BYTES,
): Promise<Readable> {
  const refuse = (reason: string): never => {
    raw.stream.destroy();
    throw new UpstreamError(502, `${label} ${reason}.`);
  };
  const type = raw.contentType.toLowerCase();
  if (type.startsWith("text/") || type.includes("json")) refuse(`is ${type}, not a ZIP archive`);
  if (raw.contentLength !== null && raw.contentLength > maximumBytes) refuse("is too large");

  const chunks = raw.stream[Symbol.asyncIterator]();
  let head = Buffer.alloc(0);
  while (head.length < ZIP_SIGNATURE.length) {
    const next = await chunks.next();
    if (next.done) break;
    head = Buffer.concat([head, Buffer.from(next.value as Uint8Array)]);
  }
  if (!head.subarray(0, ZIP_SIGNATURE.length).equals(ZIP_SIGNATURE)) refuse("is not a ZIP archive");

  async function* body(): AsyncGenerator<Buffer> {
    try {
      let total = head.length;
      if (total > maximumBytes) throw new UpstreamError(502, `${label} is too large.`);
      yield head;
      for (;;) {
        const next = await chunks.next();
        if (next.done) return;
        const chunk = Buffer.from(next.value as Uint8Array);
        total += chunk.length;
        if (total > maximumBytes) throw new UpstreamError(502, `${label} is too large.`);
        yield chunk;
      }
    } finally {
      // Stopping early (a failed cache write, the size limit) releases the upstream body.
      await chunks.return?.();
    }
  }
  return Readable.from(body(), { objectMode: false });
}
