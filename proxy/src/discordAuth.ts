// Discord login for the apps (OAuth2 authorization code flow, scope `identify` — the user's ID and
// name, nothing else; no bot and no server membership).
//
//   app                         browser                        proxy                     Discord
//   ── listens on 127.0.0.1:P,  opens /login?port=P&state=S ─▶ remembers S → P
//                                                              302 ─────────────────────▶ authorize
//                               user allows ◀────────────────────────────────────────────
//                               /callback?code&state=S ──────▶ code → token → /users/@me
//                                                              issues the session token,
//                                                              files it under a one-time ticket
//      ◀── 127.0.0.1:P/discord-login?state=S&ticket=T ◀─────── 302
//   ── /redeem?ticket=T&state=S (HMAC-signed) ───────────────▶ the session token, once
//
// The token only ever reaches the app through its own loopback listener: a link with somebody else's
// state and port, sent to a victim, sends the victim's browser to the victim's own machine, and the
// ticket is worth nothing without the state the app made up. The Discord client secret stays here.

import { randomBytes } from "node:crypto";
import type { FastifyInstance, FastifyReply, preHandlerHookHandler } from "fastify";
import type { Config } from "./config.js";
import { issueSession } from "./session.js";

const STATE_PATTERN = /^[A-Za-z0-9_-]{32,128}$/;
const TICKET_PATTERN = /^[A-Za-z0-9_-]{32,128}$/;
/** How long a started login may take at Discord. */
const PENDING_TTL_MS = 10 * 60 * 1000;
/** How long the app has to redeem a ticket after the browser came back. */
const TICKET_TTL_MS = 2 * 60 * 1000;
/** Bounds the in-memory tables, so a flood of started logins cannot grow them without end. */
const MAXIMUM_ENTRIES = 10_000;
const DISCORD_TIMEOUT_MS = 15_000;

interface DiscordUser {
  id: string;
  username: string;
  global_name?: string | null;
}

/** The two Discord calls a login needs. Separate, so tests can stand in for Discord. */
export interface DiscordApi {
  /** Exchanges an authorization code for the user it belongs to. */
  userForCode(code: string): Promise<DiscordUser>;
}

export class DiscordClient implements DiscordApi {
  constructor(private readonly config: Config) {}

  async userForCode(code: string): Promise<DiscordUser> {
    const token = await fetch(`${this.config.discordApiBase}/oauth2/token`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded", Accept: "application/json" },
      body: new URLSearchParams({
        client_id: this.config.discordClientId,
        client_secret: this.config.discordClientSecret,
        grant_type: "authorization_code",
        code,
        redirect_uri: this.config.discordRedirectUri,
      }),
      signal: AbortSignal.timeout(DISCORD_TIMEOUT_MS),
    });
    if (!token.ok) throw new Error(`Discord token exchange failed with ${token.status}`);
    const { access_token: accessToken } = (await token.json()) as { access_token?: string };
    if (!accessToken) throw new Error("Discord token exchange returned no access token");

    const me = await fetch(`${this.config.discordApiBase}/users/@me`, {
      headers: { Authorization: `Bearer ${accessToken}`, Accept: "application/json" },
      signal: AbortSignal.timeout(DISCORD_TIMEOUT_MS),
    });
    if (!me.ok) throw new Error(`Discord user lookup failed with ${me.status}`);
    const user = (await me.json()) as Partial<DiscordUser>;
    if (typeof user.id !== "string" || !/^\d{1,25}$/.test(user.id) || typeof user.username !== "string") {
      throw new Error("Discord returned no usable user");
    }
    return { id: user.id, username: user.username, global_name: user.global_name ?? null };
  }
}

/** A map whose entries expire, and which never holds more than MAXIMUM_ENTRIES. */
class Expiring<T> {
  private readonly entries = new Map<string, { value: T; expiresAt: number }>();

  constructor(private readonly ttlMs: number) {}

  set(key: string, value: T): boolean {
    this.sweep();
    if (this.entries.size >= MAXIMUM_ENTRIES) return false;
    this.entries.set(key, { value, expiresAt: Date.now() + this.ttlMs });
    return true;
  }

  /** The value under `key`, removed: every entry is good for one use. */
  take(key: string): T | undefined {
    const entry = this.entries.get(key);
    this.entries.delete(key);
    return entry && entry.expiresAt > Date.now() ? entry.value : undefined;
  }

  private sweep(): void {
    const now = Date.now();
    for (const [key, entry] of this.entries) if (entry.expiresAt <= now) this.entries.delete(key);
  }
}

function page(reply: FastifyReply, status: number, title: string, text: string): FastifyReply {
  const escape = (value: string) =>
    value.replace(/[&<>"']/g, (char) => `&#${char.charCodeAt(0)};`);
  return reply
    .code(status)
    .header("Content-Type", "text/html; charset=utf-8")
    .header("Cache-Control", "no-store")
    .send(
      `<!doctype html><meta charset="utf-8"><title>${escape(title)}</title>` +
        `<body style="font-family:system-ui,sans-serif;background:#15181b;color:#e8e6e3;display:grid;place-items:center;height:100vh;margin:0">` +
        `<div style="max-width:28rem;text-align:center"><h1 style="font-size:1.3rem">${escape(title)}</h1>` +
        `<p style="color:#8e9299">${escape(text)}</p></div>`,
    );
}

export function registerDiscordAuthRoutes(
  app: FastifyInstance,
  config: Config,
  discord: DiscordApi,
  loginAuthHook: preHandlerHookHandler,
): void {
  const available = config.discordClientId.length > 0;
  const pending = new Expiring<{ port: number }>(PENDING_TTL_MS);
  const tickets = new Expiring<{ token: string; state: string; user: { id: string; name: string }; expiresAt: number }>(
    TICKET_TTL_MS,
  );
  const authLimit = { rateLimit: { max: 20, timeWindow: 60_000 } };

  // Where the app's own listener is, back on the user's machine.
  const backToApp = (port: number, query: Record<string, string>) =>
    `http://127.0.0.1:${port}/discord-login?${new URLSearchParams(query).toString()}`;

  /** Whether login exists and is required, and who is logged in — what the app shows. */
  app.get("/v1/auth/session", { preHandler: loginAuthHook }, async (req) => ({
    login: { available, required: config.requireLogin },
    user: req.drydockUser
      ? { id: req.drydockUser.id, name: req.drydockUser.name, expires_at: req.drydockUser.expiresAt }
      : null,
  }));

  app.get<{ Querystring: { port?: string; state?: string } }>(
    "/v1/auth/discord/login",
    { config: authLimit },
    async (req, reply) => {
      if (!available) return page(reply, 503, "Login unavailable", "Discord login is not set up on this server.");
      const port = Number.parseInt(req.query.port ?? "", 10);
      const state = req.query.state ?? "";
      if (!Number.isInteger(port) || port < 1024 || port > 65535 || !STATE_PATTERN.test(state)) {
        return page(reply, 400, "Login link broken", "Start the login again from the app.");
      }
      if (!pending.set(state, { port })) {
        return page(reply, 503, "Too many logins", "Please try again in a few minutes.");
      }
      const authorize = new URLSearchParams({
        client_id: config.discordClientId,
        response_type: "code",
        redirect_uri: config.discordRedirectUri,
        scope: "identify",
        state,
        prompt: "none",
      });
      return reply.redirect(`${config.discordApiBase}/oauth2/authorize?${authorize.toString()}`, 302);
    },
  );

  app.get<{ Querystring: { code?: string; state?: string; error?: string } }>(
    "/v1/auth/discord/callback",
    { config: authLimit },
    async (req, reply) => {
      const state = req.query.state ?? "";
      const login = STATE_PATTERN.test(state) ? pending.take(state) : undefined;
      if (!login) return page(reply, 400, "Login expired", "This login is no longer valid. Start it again from the app.");
      if (req.query.error || !req.query.code) {
        return reply.redirect(backToApp(login.port, { state, error: "denied" }), 302);
      }
      let user: DiscordUser;
      try {
        user = await discord.userForCode(req.query.code);
      } catch (error) {
        req.log.warn({ err: error }, "Discord login failed.");
        return reply.redirect(backToApp(login.port, { state, error: "discord" }), 302);
      }
      if (config.bannedDiscordIds.has(user.id)) {
        req.log.warn({ discordId: user.id }, "Banned Discord account tried to log in.");
        return reply.redirect(backToApp(login.port, { state, error: "banned" }), 302);
      }
      const name = user.global_name || user.username;
      const ttlSeconds = config.sessionTtlDays * 24 * 60 * 60;
      const token = issueSession({ id: user.id, name }, config.sessionSecrets, ttlSeconds);
      const ticket = randomBytes(32).toString("base64url");
      tickets.set(ticket, {
        token,
        state,
        user: { id: user.id, name },
        expiresAt: Math.floor(Date.now() / 1000) + ttlSeconds,
      });
      req.log.info({ discordId: user.id, name }, "Discord login.");
      return reply.redirect(backToApp(login.port, { state, ticket }), 302);
    },
  );

  app.get<{ Querystring: { ticket?: string; state?: string } }>(
    "/v1/auth/discord/redeem",
    { preHandler: loginAuthHook, config: authLimit },
    async (req, reply) => {
      const ticket = req.query.ticket ?? "";
      const entry = TICKET_PATTERN.test(ticket) ? tickets.take(ticket) : undefined;
      if (!entry || entry.state !== req.query.state) return reply.code(404).send({ error: "unknown_ticket" });
      return { token: entry.token, user: entry.user, expires_at: entry.expiresAt };
    },
  );
}
