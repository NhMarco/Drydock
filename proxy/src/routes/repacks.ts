// Serves the per-app repacks list (external download sources) through the proxy.
//
//   GET /v1/repacks -> { repacks: [{ appid, sources: [{ repacker, link }] }] }
//
// The list is validated and briefly cached (repacksCache.ts). There are no file downloads here:
// each source is just an http(s) link the client opens in the user's browser.

import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import { GitHubError } from "../github.js";
import type { RepacksCache } from "../repacksCache.js";

export function registerRepacksRoute(
  app: FastifyInstance,
  cache: RepacksCache,
  authHook: preHandlerHookHandler,
): void {
  app.get("/v1/repacks", { preHandler: authHook }, async (req, reply) => {
    try {
      const repacks = await cache.getRepacks();
      return reply.code(200).header("Cache-Control", "no-store").send({ repacks });
    } catch (error) {
      return sendError(reply, req.log, error);
    }
  });
}

function sendError(reply: FastifyReply, log: FastifyBaseLogger, error: unknown): FastifyReply {
  if (error instanceof GitHubError) {
    const status = error.status === 504 ? 504 : 502;
    log.warn({ githubStatus: error.status }, "GitHub error while serving repacks.");
    return reply.code(status).send({ error: "upstream_error" });
  }
  log.error({ err: error }, "Unexpected repacks route error.");
  return reply.code(502).send({ error: "upstream_error" });
}
