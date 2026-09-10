// Relays the per-app depot package (a ZIP of `.manifest` files plus the depot key / `.lua`) that
// Drydock needs to download game files itself. The private upstream API keys stay server-side;
// clients authenticate with the Drydock HMAC hook.
//
//   GET /v1/depot/package/:appid -> application/zip (streamed)
//
// The package can come from any of several upstreams — Ryu (`secure_download`), DepotBox
// (`direct-download`), or SteamTools (`manifest`) — each returning a manifests+keys ZIP. The set and
// order are configured in `DEPOT_PACKAGE_SOURCES` (see config.ts): the route tries the enabled
// sources in order and streams the first that succeeds. The response carries `X-Depot-Source` naming
// which upstream served it. Disabling a source is purely config — the code path stays wired so it can
// be switched back on without a rebuild.

import type { Readable } from "node:stream";
import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import type { DepotPackageSourceName } from "../config.js";
import { UpstreamError, type SteamToolsClient } from "../upstream.js";
import type { DepotBoxClient, RawStream } from "../depotbox.js";
import type { RyuClient } from "../ryu.js";
import type { FileCache } from "../fileCache.js";

const APPID_PATTERN = /^[0-9]{1,10}$/;

/// A depot-package upstream: a name for `X-Depot-Source` and a function that opens its ZIP stream.
interface PackageSource {
  name: DepotPackageSourceName;
  fetch: (appid: string) => Promise<RawStream>;
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

  app.get<{ Params: { appid: string } }>(
    "/v1/depot/package/:appid",
    { preHandler: authHook },
    async (req, reply) => {
      const { appid } = req.params;
      if (!APPID_PATTERN.test(appid)) return reply.code(400).send({ error: "invalid_appid" });

      if (sources.length === 0) {
        req.log.error("No depot package sources are enabled (DEPOT_PACKAGE_SOURCES).");
        return reply.code(503).send({ error: "no_source" });
      }

      const sendZip = (
        body: NodeJS.ReadableStream,
        length: number | null,
        source: string,
        cacheState: "HIT" | "MISS",
      ) => {
        reply
          .header("Content-Type", "application/zip")
          .header("Cache-Control", "no-store")
          .header("X-Depot-Source", source)
          .header("X-Cache", cacheState)
          .header("Content-Disposition", `attachment; filename="${appid}_depot.zip"`);
        if (length !== null) reply.header("Content-Length", String(length));
        return reply.send(body);
      };

      // Serve a cached package (any source) for 24h so on-demand upstream packaging runs at most
      // once per app per day. `getOrFetch` also collapses concurrent misses for the same app into a
      // single upstream fetch — packaging a large title takes minutes, and every duplicate request
      // used to start its own job and race to write the same cache entry.
      const cacheKey = `depot:${appid}`;
      let source: DepotPackageSourceName | "cache" = "cache";
      let lastError: unknown;

      // Tries each configured upstream in order, returning the first stream that opens.
      const openUpstream = async (): Promise<Readable> => {
        for (const candidate of sources) {
          try {
            const raw: RawStream = await candidate.fetch(appid);
            source = candidate.name;
            return raw.stream;
          } catch (error) {
            lastError = error;
            req.log.info({ appid, source: candidate.name, err: error }, "Depot source failed; trying next.");
          }
        }
        throw lastError ?? new UpstreamError(502, `no depot source served ${appid}`);
      };

      try {
        const { hit, cached } = await cache.getOrFetch(cacheKey, "application/zip", openUpstream);
        return sendZip(hit.stream(), hit.contentLength, source, cached ? "HIT" : "MISS");
      } catch (error) {
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
