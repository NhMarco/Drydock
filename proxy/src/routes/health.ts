// Unauthenticated liveness/readiness endpoint for the Docker healthcheck and load balancers.
// Reports whether a usable gamelist snapshot is currently loaded.

import type { FastifyInstance } from "fastify";
import type { GamelistCache } from "../gamelistCache.js";

export function registerHealthRoute(app: FastifyInstance, cache: GamelistCache): void {
  app.get("/v1/health", { config: { rateLimit: false } }, async (_req, reply) => {
    const snapshot = cache.current;
    return reply.code(200).send({
      status: "ok",
      gamelist: snapshot
        ? { ready: true, count: snapshot.count, updatedAt: snapshot.updatedAt }
        : { ready: false },
    });
  });
}
