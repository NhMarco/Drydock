// Relays the per-app depot package (a ZIP of `.manifest` files plus the depot key / `.lua`) that
// Drydock needs to download game files itself. The private upstream API keys stay server-side;
// clients authenticate with the Drydock HMAC hook.
//
//   GET /v1/depot/package/:appid                 -> application/zip (streamed)
//   GET /v1/depot/package/:appid?source=<name>   -> the same, from that provider only
//
// The package can come from any of several upstreams — Ryu (`secure_download`), DepotBox
// (`direct-download`) or SteamTools (`manifest`) — each returning a manifests+keys ZIP. The set and
// order are configured in `PROVIDER_SOURCES` (see config.ts): without `source` the route tries the
// enabled providers in order and caches the first real ZIP archive one returns; a source that answers
// with anything else, or that is rate limited, is skipped. With `source` the user picked the provider
// in the app, so only that one is asked (and cached apart). The response carries `X-Depot-Source`
// naming which upstream served it and `X-Cache` saying whether it came from the cache.
//
// The `.lua` in a package loses the provider's comments and gets Drydock's header (luaClean.ts)
// before it is cached, so every client gets the same clean file whichever provider served it.

import { Readable } from "node:stream";
import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import type { DepotPackageSourceName } from "../config.js";
import { UpstreamError, type SteamToolsClient } from "../upstream.js";
import type { DepotBoxClient, RawStream } from "../depotbox.js";
import type { RyuClient } from "../ryu.js";
import type { CacheHit, FileCache } from "../fileCache.js";
import type { ProviderCooldown } from "../merged.js";
import { MAXIMUM_PACKAGE_BYTES, requireZip } from "../packageCheck.js";
import { cleanPackageLuas } from "../luaClean.js";

const APPID_PATTERN = /^[0-9]{1,10}$/;

// A package cached before this process started may predate Lua cleaning. Such a copy is cleaned as
// it is served — cleaning is idempotent, so one already clean passes unchanged — until a fresh fetch
// replaces it within a day.
const STARTED_AT = Date.now();

async function collect(stream: NodeJS.ReadableStream): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk as Uint8Array));
  return Buffer.concat(chunks);
}

/// A depot-package upstream: a name for `X-Depot-Source` and a function that opens its ZIP stream.
interface PackageSource {
  name: DepotPackageSourceName;
  fetch: (appid: string) => Promise<RawStream>;
}

export interface DepotLimits {
  /** Packages one client may ask for per window. */
  rateMax: number;
  rateWindowMs: number;
}

export interface DepotUpstreams {
  ryu: RyuClient;
  depotbox: DepotBoxClient;
  steamtools: SteamToolsClient;
}

export function registerDepotRoutes(
  app: FastifyInstance,
  upstreams: DepotUpstreams,
  enabledSources: DepotPackageSourceName[],
  cache: FileCache,
  authHook: preHandlerHookHandler,
  cooldown: ProviderCooldown,
  limits: DepotLimits = { rateMax: 20, rateWindowMs: 60_000 },
): void {
  // Map each configured source name to its fetch function, preserving the configured order. Sources
  // that aren't enabled simply aren't in this list.
  const factories: Record<DepotPackageSourceName, PackageSource["fetch"]> = {
    ryu: (appid) => upstreams.ryu.downloadDepotPackage(appid),
    depotbox: (appid) => upstreams.depotbox.downloadDepotPackage(appid),
    steamtools: (appid) => upstreams.steamtools.downloadManifestZip(appid),
  };
  const sources: PackageSource[] = enabledSources.map((name) => ({ name, fetch: factories[name] }));
  app.log.info({ sources: enabledSources }, "Depot package sources enabled.");

  app.get<{ Params: { appid: string }; Querystring: { source?: string } }>(
    "/v1/depot/package/:appid",
    { preHandler: authHook, config: { rateLimit: { max: limits.rateMax, timeWindow: limits.rateWindowMs } } },
    async (req, reply) => {
      const { appid } = req.params;
      if (!APPID_PATTERN.test(appid)) return reply.code(400).send({ error: "invalid_appid" });

      if (sources.length === 0) {
        req.log.error("No depot package sources are enabled (PROVIDER_SOURCES).");
        return reply.code(503).send({ error: "no_source" });
      }
      const chosen = req.query.source;
      const candidates = chosen === undefined ? sources : sources.filter((source) => source.name === chosen);
      if (candidates.length === 0) return reply.code(400).send({ error: "unknown_source" });

      // A cached copy from before this process started, cleaned on the way out (see STARTED_AT).
      const serveCached = async (hit: CacheHit, source: string, cacheState: "HIT" | "MISS") => {
        if (hit.storedAt >= STARTED_AT) return sendZip(hit.stream(), hit.contentLength, source, cacheState);
        const cleaned = cleanPackageLuas(await collect(hit.stream()), appid);
        return sendZip(Readable.from([cleaned]), cleaned.length, source, cacheState);
      };

      const sendZip = (body: NodeJS.ReadableStream, length: number | null, source: string, cacheState: "HIT" | "MISS") => {
        reply
          .header("Content-Type", "application/zip")
          .header("Cache-Control", "no-store")
          .header("X-Depot-Source", source)
          .header("X-Cache", cacheState)
          .header("Content-Disposition", `attachment; filename="${appid}_depot.zip"`);
        if (length !== null) reply.header("Content-Length", String(length));
        return reply.send(body);
      };

      // Serve a cached package for 24h so on-demand upstream packaging runs at most once per app (and
      // chosen source) per day. `getOrFetch` also collapses concurrent misses for the same key into a
      // single upstream fetch — packaging a large title takes minutes, and every duplicate request
      // used to start its own job and race to write the same cache entry.
      const cacheKey = chosen === undefined ? `depot:${appid}` : `depot:${appid}:${chosen}`;
      let source: DepotPackageSourceName | "cache" = "cache";
      let lastError: unknown;

      // Tries the candidates in order, returning the first stream that opens.
      const openUpstream = async (): Promise<Readable> => {
        for (const candidate of candidates) {
          if (cooldown.isCooling(candidate.name)) {
            lastError ??= new UpstreamError(429, `${candidate.name} is rate limited`);
            continue;
          }
          try {
            const raw: RawStream = await candidate.fetch(appid);
            // A package with no `.manifest` in it is a provider saying it has no depot data for
            // this app, in a shape that would otherwise be cached for a day — and that would shut
            // out the provider that does have it.
            const stream = await requireZip(
              raw,
              `${candidate.name} depot package for ${appid}`,
              MAXIMUM_PACKAGE_BYTES,
              true,
            );
            // Read whole (requireZip bounds it) so the Lua inside can be cleaned before caching.
            const cleaned = cleanPackageLuas(await collect(stream), appid);
            source = candidate.name;
            return Readable.from([cleaned]);
          } catch (error) {
            cooldown.noteFailure(candidate.name, error);
            lastError = error;
            req.log.info({ appid, source: candidate.name, err: error }, "Depot source failed; trying next.");
          }
        }
        throw lastError ?? new UpstreamError(502, `no depot source served ${appid}`);
      };

      try {
        const { hit, cached } = await cache.getOrFetch(cacheKey, "application/zip", openUpstream);
        return await serveCached(hit, cached ? "cache" : source, cached ? "HIT" : "MISS");
      } catch (error) {
        // Every candidate failed. An expired copy on disk is a better answer than an error, and it
        // costs nobody anything.
        const stale = await cache.getStale(cacheKey);
        if (stale) {
          req.log.info({ appid, storedAt: stale.storedAt }, "No provider served the package; serving the copy on disk.");
          return await serveCached(stale, "cache", "HIT");
        }
        // Deliberately *not* falling back to piping `raw.stream` here: by the time a cache write
        // fails its pipeline has already consumed part (or all) of the upstream stream, so replaying
        // it would send a silently truncated ZIP under a correct-looking Content-Length. A clean
        // error the client can retry is the only honest answer.
        return sendError(reply, req.log, error, `depot package ${appid}`);
      }
    },
  );
}

function sendError(reply: FastifyReply, log: FastifyBaseLogger, error: unknown, context: string): FastifyReply {
  if (error instanceof UpstreamError) {
    const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
    log.warn({ context, upstreamStatus: error.status }, "Depot upstream error.");
    return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
  }
  log.error({ err: error, context }, "Unexpected depot route error.");
  return reply.code(502).send({ error: "upstream_error" });
}
