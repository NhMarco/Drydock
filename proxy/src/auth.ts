// Fastify preHandler that enforces HMAC auth on protected routes. When REQUIRE_AUTH is
// disabled (local testing only) it becomes a no-op.
//
// On top of the HMAC, a request can carry a session token from the Discord login (session.ts). It is
// read for every request (so the rate limits can count per account rather than per address) and, once
// REQUIRE_LOGIN is on, demanded: the HMAC secret alone — readable out of any app build — then gets
// nobody anywhere.
//
// This is an async hook: to stop the request from reaching the route handler we must
// `return reply` after sending, which is how Fastify signals "the hook already responded".

import type { FastifyInstance, FastifyReply, FastifyRequest, preHandlerHookHandler } from "fastify";
import type { Config } from "./config.js";
import { NonceStore, verifyHmac } from "./hmac.js";
import { bearerToken, verifySession, type SessionUser } from "./session.js";

declare module "fastify" {
  interface FastifyRequest {
    /** The logged-in user, when the request carries a valid session token. */
    drydockUser?: SessionUser;
  }
}

/**
 * Reads the session token of every request before anything else runs, so the rate limiter can key on
 * the account. A missing or invalid token just leaves the request anonymous; whether that is allowed
 * is the auth hook's call.
 */
export function attachSession(app: FastifyInstance, config: Config): void {
  app.addHook("onRequest", async (req) => {
    if (config.sessionSecrets.length === 0) return;
    const token = bearerToken(req.headers.authorization);
    if (!token) return;
    const user = verifySession(token, config.sessionSecrets, config.bannedDiscordIds);
    if (user) req.drydockUser = user;
  });
}

/** What the rate limits count by: the account when logged in, the address otherwise. */
export function clientKey(req: FastifyRequest): string {
  return req.drydockUser ? `user:${req.drydockUser.id}` : `ip:${req.ip}`;
}

export interface AuthOptions {
  /** For the login itself: the app cannot have a session before it has logged in. */
  allowWithoutLogin?: boolean;
}

export function createAuthHook(config: Config, options: AuthOptions = {}): preHandlerHookHandler {
  const nonceStore = new NonceStore(config.hmacWindowSeconds);

  return async function authHook(req: FastifyRequest, reply: FastifyReply): Promise<FastifyReply | void> {
    if (!config.requireAuth) return;

    const result = verifyHmac(req, {
      secrets: config.hmacSecrets,
      windowSeconds: config.hmacWindowSeconds,
      nonceStore,
    });

    if (!result.ok) {
      // Log the reason for abuse analysis but never leak it to the caller.
      req.log.warn({ ip: req.ip, path: req.url, reason: result.reason }, "Rejected unauthenticated request.");
      reply.code(401).send({ error: "unauthorized" });
      return reply;
    }

    if (config.requireLogin && !options.allowWithoutLogin && !req.drydockUser) {
      // The app shows its Discord login on this answer.
      reply.code(401).send({ error: "login_required" });
      return reply;
    }

    return;
  };
}
