// Fastify preHandler that enforces HMAC auth on protected routes. When REQUIRE_AUTH is
// disabled (local testing only) it becomes a no-op.
//
// This is an async hook: to stop the request from reaching the route handler we must
// `return reply` after sending, which is how Fastify signals "the hook already responded".

import type { FastifyReply, FastifyRequest, preHandlerHookHandler } from "fastify";
import type { Config } from "./config.js";
import { NonceStore, verifyHmac } from "./hmac.js";

export function createAuthHook(config: Config): preHandlerHookHandler {
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

    return;
  };
}
