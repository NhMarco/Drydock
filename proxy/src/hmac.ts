// HMAC request authentication. The Drydock app embeds a shared secret at publish time and
// signs every protected request. This cannot make an extractable secret unextractable, but
// combined with a tight timestamp window and a replay cache it raises the bar well past a
// trivial scraper and lets us revoke a leaked secret by rotating DRYDOCK_HMAC_SECRET.
//
// Canonical signing string (exactly, LF-separated, no trailing newline):
//   METHOD + "\n" + PATH_AND_QUERY + "\n" + TIMESTAMP + "\n" + NONCE
// Signature = Base64( HMAC_SHA256(secret, signingString) )
//
// Required headers on each request:
//   X-Drydock-Timestamp : Unix seconds (as sent by the client)
//   X-Drydock-Nonce     : unique random token per request (hex, 16-64 chars)
//   X-Drydock-Signature : Base64 HMAC as above

import { createHmac, timingSafeEqual } from "node:crypto";
import type { FastifyRequest } from "fastify";

export type AuthResult =
  | { ok: true }
  | { ok: false; reason: string };

export function buildSigningString(method: string, pathAndQuery: string, timestamp: string, nonce: string): string {
  return `${method.toUpperCase()}\n${pathAndQuery}\n${timestamp}\n${nonce}`;
}

function computeSignature(secret: string, signingString: string): Buffer {
  return createHmac("sha256", secret).update(signingString, "utf8").digest();
}

function safeEqualBase64(candidate: string, expected: Buffer): boolean {
  let provided: Buffer;
  try {
    provided = Buffer.from(candidate, "base64");
  } catch {
    return false;
  }
  if (provided.length !== expected.length || provided.length === 0) return false;
  return timingSafeEqual(provided, expected);
}

// Remembers recently seen nonces so a captured request cannot be replayed within the
// validity window. Entries expire on their own; a periodic sweep bounds memory.
export class NonceStore {
  private readonly seen = new Map<string, number>();
  private readonly ttlMs: number;

  constructor(windowSeconds: number) {
    // Keep nonces a little longer than the timestamp window so nothing slips through the seam.
    this.ttlMs = (windowSeconds * 2 + 5) * 1000;
    // Sweep on a fixed short cadence rather than once per TTL: with a 300s window the old
    // `max(ttlMs, 30s)` only ran every ~10 minutes, so the map held ten minutes of nonces even
    // though each one becomes useless much sooner.
    const timer = setInterval(() => this.sweep(), 60_000);
    timer.unref();
  }

  // Returns true if this nonce is fresh (and records it), false if it was already used.
  remember(nonce: string): boolean {
    const now = Date.now();
    const existing = this.seen.get(nonce);
    if (existing !== undefined && existing > now) return false;
    this.seen.set(nonce, now + this.ttlMs);
    return true;
  }

  private sweep(): void {
    const now = Date.now();
    for (const [nonce, expiry] of this.seen) {
      if (expiry <= now) this.seen.delete(nonce);
    }
  }
}

export interface HmacVerifierOptions {
  secrets: string[];
  windowSeconds: number;
  nonceStore: NonceStore;
}

const NONCE_PATTERN = /^[A-Za-z0-9_-]{16,64}$/;

export function verifyHmac(req: FastifyRequest, options: HmacVerifierOptions): AuthResult {
  const timestamp = firstHeader(req.headers["x-drydock-timestamp"]);
  const nonce = firstHeader(req.headers["x-drydock-nonce"]);
  const signature = firstHeader(req.headers["x-drydock-signature"]);

  if (!timestamp || !nonce || !signature) return { ok: false, reason: "missing_auth_headers" };
  if (!NONCE_PATTERN.test(nonce)) return { ok: false, reason: "invalid_nonce" };

  const timestampSeconds = Number.parseInt(timestamp, 10);
  if (!Number.isFinite(timestampSeconds)) return { ok: false, reason: "invalid_timestamp" };

  const nowSeconds = Math.floor(Date.now() / 1000);
  if (Math.abs(nowSeconds - timestampSeconds) > options.windowSeconds) {
    return { ok: false, reason: "stale_timestamp" };
  }

  // req.url is the origin-form path + query, which is exactly what the client signs.
  const signingString = buildSigningString(req.method, req.url, timestamp, nonce);

  const matches = options.secrets.some((secret) =>
    safeEqualBase64(signature, computeSignature(secret, signingString)),
  );
  if (!matches) return { ok: false, reason: "bad_signature" };

  // Only burn the nonce once the signature is valid, so bogus traffic cannot poison the cache.
  if (!options.nonceStore.remember(nonce)) return { ok: false, reason: "replayed_nonce" };

  return { ok: true };
}

function firstHeader(value: string | string[] | undefined): string | undefined {
  if (Array.isArray(value)) return value[0];
  return value;
}
