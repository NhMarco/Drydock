// Session tokens: what the proxy hands an app once its user has logged in with Discord.
//
// The HMAC secret that signs every request ships inside every app build, so anyone can read it out
// and call the proxy as if they were the app. A session token cannot be read out of anything: it is
// issued to one person after a Discord login, names their Discord account, and is signed with a key
// that never leaves the server. That is what makes abuse attributable, limitable and bannable.
//
// Format: `dl1.<payload>.<signature>`, both parts base64url. The payload is JSON
// `{ "uid": "<discord id>", "name": "<display name>", "iat": <unix s>, "exp": <unix s> }`, the
// signature HMAC-SHA256 over `dl1.<payload>` with the first `SESSION_SECRET`; every listed secret is
// accepted, so the key can be rotated without logging everyone out at once. Stateless: verifying one
// needs no lookup but the ban list.

import { createHmac, timingSafeEqual } from "node:crypto";

const PREFIX = "dl1";
// A token is far shorter than this; anything longer is not one and is not worth a HMAC.
const MAXIMUM_TOKEN_LENGTH = 1024;

export interface SessionUser {
  /** The Discord user ID. */
  id: string;
  /** What to call them (Discord display name or username), for the app and the logs. */
  name: string;
  /** When the session ends, Unix seconds. */
  expiresAt: number;
}

function sign(secret: string, data: string): Buffer {
  return createHmac("sha256", secret).update(data, "utf8").digest();
}

/** A new session token for `user`, valid for `ttlSeconds` from `now` (Unix seconds). */
export function issueSession(
  user: { id: string; name: string },
  secrets: string[],
  ttlSeconds: number,
  now = Math.floor(Date.now() / 1000),
): string {
  const secret = secrets[0];
  if (!secret) throw new Error("No session secret configured");
  const payload = Buffer.from(
    JSON.stringify({ uid: user.id, name: user.name, iat: now, exp: now + ttlSeconds }),
    "utf8",
  ).toString("base64url");
  const signed = `${PREFIX}.${payload}`;
  return `${signed}.${sign(secret, signed).toString("base64url")}`;
}

/**
 * The user a token belongs to, or null when it is malformed, signed with no current secret,
 * expired, or belongs to a banned account.
 */
export function verifySession(
  token: string,
  secrets: string[],
  banned: ReadonlySet<string>,
  now = Math.floor(Date.now() / 1000),
): SessionUser | null {
  if (token.length > MAXIMUM_TOKEN_LENGTH) return null;
  const parts = token.split(".");
  if (parts.length !== 3 || parts[0] !== PREFIX) return null;
  const [, payload, signature] = parts as [string, string, string];
  const given = Buffer.from(signature, "base64url");
  const signed = `${PREFIX}.${payload}`;
  const valid = secrets.some((secret) => {
    const expected = sign(secret, signed);
    return expected.length === given.length && timingSafeEqual(expected, given);
  });
  if (!valid) return null;
  let claims: unknown;
  try {
    claims = JSON.parse(Buffer.from(payload, "base64url").toString("utf8"));
  } catch {
    return null;
  }
  if (typeof claims !== "object" || claims === null) return null;
  const { uid, name, exp } = claims as Record<string, unknown>;
  if (typeof uid !== "string" || !/^\d{1,25}$/.test(uid)) return null;
  if (typeof exp !== "number" || exp <= now) return null;
  if (banned.has(uid)) return null;
  return { id: uid, name: typeof name === "string" ? name : uid, expiresAt: exp };
}

/** The bearer token of a request, if it carries one. */
export function bearerToken(authorization: string | undefined): string | null {
  if (!authorization) return null;
  const match = /^Bearer\s+(\S+)$/i.exec(authorization.trim());
  return match?.[1] ?? null;
}
