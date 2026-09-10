// Serves the cached, compacted, gzipped gamelist. Clients send If-None-Match with the
// previously received ETag and get a 304 when nothing changed, so a client that already
// has the current catalog transfers almost nothing.

import type { FastifyInstance } from "fastify";
import type { Config } from "../config.js";
import type { GamelistCache } from "../gamelistCache.js";
import type { preHandlerHookHandler } from "fastify";

export function registerGamelistRoute(
  app: FastifyInstance,
  config: Config,
  cache: GamelistCache,
  authHook: preHandlerHookHandler,
): void {
  app.get(
    "/v1/gamelist",
    {
      preHandler: authHook,
      config: {
        // Cheap to serve from cache; a client refetch once per interval is normal, so the
        // limit only exists to blunt a client hammering it in a tight loop.
        rateLimit: {
          max: Math.max(config.globalRateMax, 10),
          timeWindow: config.globalRateWindowSeconds * 1000,
        },
      },
    },
    async (req, reply) => {
      const snapshot = cache.current;
      if (!snapshot) {
        return reply.code(503).header("Retry-After", "30").send({ error: "gamelist_unavailable" });
      }

      const ifNoneMatch = req.headers["if-none-match"];
      if (ifNoneMatch && ifNoneMatch === snapshot.etag) {
        return reply
          .code(304)
          .header("ETag", snapshot.etag)
          .header("Cache-Control", "public, max-age=300")
          .send();
      }

      return reply
        .code(200)
        .header("Content-Type", "application/json; charset=utf-8")
        .header("Content-Encoding", "gzip")
        .header("ETag", snapshot.etag)
        .header("Cache-Control", "public, max-age=300")
        .header("X-Gamelist-Count", String(snapshot.count))
        .header("X-Gamelist-Updated", String(snapshot.updatedAt))
        .send(snapshot.gzipBody);
    },
  );
}
