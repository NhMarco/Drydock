// Checks a depot package before it is cached. A provider can answer status 200 with an HTML error
// page, a Cloudflare challenge or a JSON error; cached, that would be handed to every client for a
// day while the next provider, which might have the real package, is never asked.

import { Readable } from "node:stream";
import type { RawStream } from "./depotbox.js";
import { UpstreamError } from "./upstream.js";

const ZIP_SIGNATURE = Buffer.from([0x50, 0x4b, 0x03, 0x04]);
const LOCAL_FILE_HEADER = 0x04034b50;

// The same ceiling the Drydock client applies to a depot package.
export const MAXIMUM_PACKAGE_BYTES = 256 * 1024 * 1024;

// How much of an archive is read before it is accepted as having real content. Entry names sit in
// the local file header that precedes each file, so a package's `.manifest` entries are named
// within the first few hundred bytes; an archive that reaches this size without one is far past
// anything a lua-only answer could be, and is passed through rather than held up.
const CONTENT_SCAN_BYTES = 1024 * 1024;

/** The entry names visible in `buffer`, read from the ZIP local file headers. */
function entryNames(buffer: Buffer): string[] {
  const names: string[] = [];
  for (let offset = 0; offset + 30 <= buffer.length; offset += 1) {
    if (buffer.readUInt32LE(offset) !== LOCAL_FILE_HEADER) continue;
    const length = buffer.readUInt16LE(offset + 26);
    if (offset + 30 + length > buffer.length) continue;
    names.push(buffer.subarray(offset + 30, offset + 30 + length).toString("utf8"));
  }
  return names;
}

// Returns the package body once its content type and first bytes show a ZIP archive. The returned
// stream still yields the whole body, and fails if it grows past `maximumBytes`.
//
// With `requireManifest`, an archive that turns out to hold no `.manifest` at all is refused as
// well. A provider answering with just the app's `.lua` is answering "I do not have this app's
// depot data" in a shape that passes every other check — and cached, that shuts out the provider
// that does have it, which for a last-resort source means the app cannot be downloaded at all.
export async function requireZip(
  raw: RawStream,
  label: string,
  maximumBytes = MAXIMUM_PACKAGE_BYTES,
  requireManifest = false,
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
  let ended = false;
  // Enough for the signature, and — when the content has to be checked — enough of the archive to
  // see its entry names.
  const wanted = requireManifest ? CONTENT_SCAN_BYTES : ZIP_SIGNATURE.length;
  while (head.length < wanted) {
    const next = await chunks.next();
    if (next.done) {
      ended = true;
      break;
    }
    head = Buffer.concat([head, Buffer.from(next.value as Uint8Array)]);
  }
  if (!head.subarray(0, ZIP_SIGNATURE.length).equals(ZIP_SIGNATURE)) refuse("is not a ZIP archive");
  // Only decided for an archive read to its end: a bigger one is real content by any measure.
  if (requireManifest && ended && !entryNames(head).some((name) => name.toLowerCase().endsWith(".manifest"))) {
    refuse("holds no depot manifest");
  }

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
