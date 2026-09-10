// On-demand per-app enrichment. The flat gamelist has no type/DRM, so the client fetches this
// for the app the user opened to learn its app_type, technologies (incl. Denuvo), and richer
// metadata. Cached per AppID for a long TTL since it changes rarely.

import type { FastifyInstance, preHandlerHookHandler } from "fastify";
import type { Config } from "../config.js";
import { SteamToolsClient, UpstreamError } from "../upstream.js";

interface CachedInfo {
  body: string;
  contentType: string;
  expiresAt: number;
}

const APPID_PATTERN = /^[1-9][0-9]{0,7}$/;

export function registerAppInfoRoute(
  app: FastifyInstance,
  config: Config,
  client: SteamToolsClient,
  authHook: preHandlerHookHandler,
): void {
  const cache = new Map<string, CachedInfo>();
  const sweep = setInterval(() => {
    const now = Date.now();
    for (const [key, value] of cache) if (value.expiresAt <= now) cache.delete(key);
  }, 300_000);
  sweep.unref();

  app.get<{ Params: { appid: string } }>(
    "/v1/app-info/:appid",
    { preHandler: authHook },
    async (req, reply) => {
      const { appid } = req.params;
      if (!APPID_PATTERN.test(appid)) return reply.code(400).send({ error: "invalid_appid" });

      const now = Date.now();
      const cached = cache.get(appid);
      if (cached && cached.expiresAt > now) {
        return reply.code(200).header("Content-Type", cached.contentType).header("X-Cache", "HIT").send(cached.body);
      }

      try {
        const result = await client.fetchAppInfo(appid);
        if (config.appInfoCacheTtlSeconds > 0) {
          cache.set(appid, { body: result.body, contentType: result.contentType, expiresAt: now + config.appInfoCacheTtlSeconds * 1000 });
        }
        return reply.code(200).header("Content-Type", result.contentType).header("X-Cache", "MISS").send(result.body);
      } catch (error) {
        if (error instanceof UpstreamError) {
          const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
          req.log.warn({ appid, upstreamStatus: error.status }, "App-info upstream error.");
          return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
        }
        req.log.error({ err: error, appid }, "Unexpected app-info route error.");
        return reply.code(502).send({ error: "upstream_error" });
      }
    },
  );
}
