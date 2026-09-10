// Per-app achievement schema for the local emulator-template generator. The Steam Web API key
// lives only here (server-side); the client asks the proxy, never Steam directly. Returns the
// gbe_fork `achievements.json` array (icons as full CDN URLs), or `[]` when there is no key, no
// achievements, or the upstream fails — the generator treats an empty list as "no achievements".
//
// A successfully built schema is stored in the shared 24h disk `FileCache`, so the Steam Web API is
// hit at most once per app per day and the cache survives proxy restarts (unlike the old in-memory
// map). Empty `[]` fallbacks from a missing key / upstream failure are NOT cached, so a transient
// error isn't pinned for a day.

import { Readable } from "node:stream";

import type { FastifyInstance, preHandlerHookHandler } from "fastify";
import type { Config } from "../config.js";
import type { FileCache } from "../fileCache.js";

const APPID_PATTERN = /^[1-9][0-9]{0,7}$/;
const SCHEMA_CONTENT_TYPE = "application/json; charset=utf-8";

export function registerSchemaRoute(
  app: FastifyInstance,
  config: Config,
  fileCache: FileCache,
  authHook: preHandlerHookHandler,
): void {
  app.get<{ Params: { appid: string } }>(
    "/v1/app-schema/:appid",
    { preHandler: authHook },
    async (req, reply) => {
      const { appid } = req.params;
      if (!APPID_PATTERN.test(appid)) return reply.code(400).send({ error: "invalid_appid" });

      // An uncached empty list (no key, upstream failure) — never stored, so it retries next time.
      const emptyUncached = () =>
        reply.code(200).header("Content-Type", SCHEMA_CONTENT_TYPE).header("X-Cache", "MISS").send("[]");

      if (!config.steamWebApiKey) return emptyUncached();

      const cacheKey = `app-schema:${appid}`;
      const cached = await fileCache.get(cacheKey);
      if (cached) {
        return reply
          .code(200)
          .header("Content-Type", cached.contentType)
          .header("Content-Length", cached.contentLength)
          .header("X-Cache", "HIT")
          .send(cached.stream());
      }

      const cdn = `https://steamcdn-a.akamaihd.net/steamcommunity/public/images/apps/${appid}/`;
      const iconUrl = (value: unknown): string => {
        const raw = String(value ?? "").trim();
        if (!raw) return "";
        if (raw.startsWith("http://") || raw.startsWith("https://")) return raw;
        return cdn + raw + (raw.endsWith(".jpg") ? "" : ".jpg");
      };

      try {
        const url =
          "https://api.steampowered.com/ISteamUserStats/GetSchemaForGame/v2/" +
          `?key=${encodeURIComponent(config.steamWebApiKey)}&appid=${appid}&l=english`;
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), 30_000);
        let payload: unknown;
        try {
          const response = await fetch(url, { signal: controller.signal });
          if (!response.ok) return emptyUncached();
          payload = await response.json();
        } finally {
          clearTimeout(timer);
        }
        const stats =
          (payload as { game?: { availableGameStats?: { achievements?: unknown[] } } })?.game
            ?.availableGameStats?.achievements ?? [];
        const achievements = (Array.isArray(stats) ? stats : [])
          .filter((entry): entry is Record<string, unknown> => typeof entry === "object" && entry !== null)
          .map((entry) => ({
            name: String(entry.name ?? ""),
            defaultvalue: Number(entry.defaultvalue ?? 0) || 0,
            displayName: String(entry.displayName ?? ""),
            hidden: Number(entry.hidden ?? 0) || 0,
            description: String(entry.description ?? ""),
            icon: iconUrl(entry.icon),
            icongray: iconUrl(entry.icongray),
          }));
        const body = JSON.stringify(achievements, null, 4);

        // Cache the successful schema on disk and serve it back. If the cache write fails, still
        // return the freshly built body rather than erroring.
        //
        // This fallback is safe here — unlike the depot/magicfiles routes, which must not retry after
        // a failed `put` — because `body` is a complete in-memory string: a fresh `Readable.from`
        // replays it in full, whereas a consumed upstream stream would send truncated bytes.
        try {
          const stored = await fileCache.put(cacheKey, Readable.from(body), SCHEMA_CONTENT_TYPE);
          return reply
            .code(200)
            .header("Content-Type", stored.contentType)
            .header("Content-Length", stored.contentLength)
            .header("X-Cache", "MISS")
            .send(stored.stream());
        } catch {
          return reply
            .code(200)
            .header("Content-Type", SCHEMA_CONTENT_TYPE)
            .header("X-Cache", "MISS")
            .send(body);
        }
      } catch (error) {
        req.log.warn({ appid, err: error }, "App-schema upstream error; returning empty list.");
        return emptyUncached();
      }
    },
  );
}
