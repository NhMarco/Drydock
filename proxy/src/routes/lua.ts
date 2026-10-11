// Proxies a single generated .lua by AppID. This is the expensive upstream call, so it
// carries a per-client rate limit (30/min by default) and a short server-side cache so a
// popular title does not trigger one upstream generation per client.

import type { FastifyInstance, preHandlerHookHandler } from "fastify";
import type { Config } from "../config.js";
import type { LuaSource } from "../depotbox.js";
import type { LuaResult } from "../upstream.js";
import { UpstreamError } from "../upstream.js";
import { sanitizeLua } from "../luaSanitize.js";
import { cleanLua } from "../luaClean.js";

interface CachedLua {
  body: string;
  contentType: string;
  expiresAt: number;
}

// AppIDs are numeric and bounded; reject anything else before it reaches upstream.
const APPID_PATTERN = /^[1-9][0-9]{0,7}$/;

/** A Lua source the user can also pick a single provider of (`?source=`), as `MergedLuaSource`. */
interface ChoosableLuaSource extends LuaSource {
  names?: () => string[];
  fetchLua(appid: string, only?: string): Promise<LuaResult>;
}

export function registerLuaRoute(
  app: FastifyInstance,
  config: Config,
  client: ChoosableLuaSource,
  authHook: preHandlerHookHandler,
): void {
  const cache = new Map<string, CachedLua>();

  const sweep = setInterval(() => {
    const now = Date.now();
    for (const [key, value] of cache) if (value.expiresAt <= now) cache.delete(key);
  }, 60_000);
  sweep.unref();

  app.get<{ Params: { appid: string }; Querystring: { source?: string } }>(
    "/v1/lua/:appid",
    {
      preHandler: authHook,
      config: {
        rateLimit: {
          max: config.luaRateMax,
          timeWindow: config.luaRateWindowSeconds * 1000,
        },
      },
    },
    async (req, reply) => {
      const { appid } = req.params;
      if (!APPID_PATTERN.test(appid)) {
        return reply.code(400).send({ error: "invalid_appid" });
      }
      // `source`: the provider the user picked in the app, asked alone and cached apart.
      const chosen = req.query.source;
      if (chosen !== undefined && !(client.names?.() ?? []).includes(chosen)) {
        return reply.code(400).send({ error: "unknown_source" });
      }
      const key = chosen === undefined ? appid : `${appid}:${chosen}`;

      const now = Date.now();
      const cached = cache.get(key);
      if (cached && cached.expiresAt > now) {
        return reply
          .code(200)
          .header("Content-Type", cached.contentType)
          .header("X-Cache", "HIT")
          .send(cached.body);
      }

      try {
        const result = await client.fetchLua(appid, chosen);
        // Drop broken keyless `addappid(<depotid>)` depot lines the upstream sometimes emits; they
        // make the whole unlock fail. Then the provider's comments go and Drydock's header goes on —
        // in that order, since the sanitizer reads commented-out `setManifestid` lines. The cached +
        // served copy is the cleaned one.
        const sanitized = sanitizeLua(result.body);
        const { removed } = sanitized;
        const body = cleanLua(sanitized.body, appid);
        if (removed > 0) {
          req.log.info({ appid, removed }, "Stripped keyless addappid depot line(s) from upstream Lua.");
        }
        if (config.luaCacheTtlSeconds > 0) {
          cache.set(key, {
            body,
            contentType: result.contentType,
            expiresAt: now + config.luaCacheTtlSeconds * 1000,
          });
        }
        return reply
          .code(200)
          .header("Content-Type", result.contentType)
          .header("X-Cache", "MISS")
          .send(body);
      } catch (error) {
        if (error instanceof UpstreamError) {
          // Map upstream 404 to a clean 404; collapse everything else to 502/504 without
          // echoing upstream detail to the client.
          const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
          req.log.warn({ appid, upstreamStatus: error.status }, "Lua upstream error.");
          return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
        }
        req.log.error({ err: error, appid }, "Unexpected lua route error.");
        return reply.code(502).send({ error: "upstream_error" });
      }
    },
  );
}
