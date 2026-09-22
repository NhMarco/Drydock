// Relays the per-app depot package (a ZIP of `.manifest` files plus the depot key / `.lua`) that
// Drydock needs to download game files itself. The private upstream API keys stay server-side;
// clients authenticate with the Drydock HMAC hook.
//
//   GET /v1/depot/package/:appid -> application/zip (streamed)
//
// The package can come from any of several upstreams — Ryu (`secure_download`), DepotBox
// (`direct-download`), SteamTools (`manifest`) or Hubcap (`api/v1/manifest`) — each returning a
// manifests+keys ZIP. The set and order are configured in `PROVIDER_SOURCES` (see config.ts): the
// route tries the enabled sources in order and caches the first real ZIP archive one returns. A
// source that answers with anything else, or that is rate limited, is skipped. The response carries
// `X-Depot-Source` naming which upstream served it. Disabling a source is purely config — the code
// path stays wired so it can be switched back on without a rebuild.
//
// Hubcap is a **day-limited** source (a handful of manifest downloads per day), so it sits apart
// from that order:
//
//   * it is asked only after every ordinary provider failed, wherever it is listed;
//   * it is not asked at all while a copy of that app is on disk, however old, or while its
//     allowance is down to the configured reserve;
//   * what it does serve is **kept**: that cache entry outlives the TTL and the sweep, so the app
//     stays downloadable without spending the allowance again. The moment an ordinary provider
//     serves the same app, its package replaces the kept one and normal 24h caching resumes.
//
// `X-Cache` says which of the three a response came from: `MISS` (fetched now), `HIT` (cached
// normally) or `KEPT` (the retained copy from a day-limited source).

import type { Readable } from "node:stream";
import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import { isLastResort, orderedSources, type DepotPackageSourceName } from "../config.js";
import { UpstreamError, type SteamToolsClient } from "../upstream.js";
import type { DepotBoxClient, RawStream } from "../depotbox.js";
import type { RyuClient } from "../ryu.js";
import type { HubcapClient } from "../hubcap.js";
import type { FileCache } from "../fileCache.js";
import type { ProviderCooldown } from "../merged.js";
import { MAXIMUM_PACKAGE_BYTES, requireZip } from "../packageCheck.js";

const APPID_PATTERN = /^[0-9]{1,10}$/;

/// A depot-package upstream: a name for `X-Depot-Source` and a function that opens its ZIP stream.
interface PackageSource {
  name: DepotPackageSourceName;
  fetch: (appid: string) => Promise<RawStream>;
  /** Day-limited: only asked once everything else failed, and what it serves is kept (see below). */
  lastResort: boolean;
  /** Whether its allowance permits a request right now. */
  allowed: () => Promise<boolean>;
}

export interface DepotUpstreams {
  ryu: RyuClient;
  depotbox: DepotBoxClient;
  steamtools: SteamToolsClient;
  hubcap: HubcapClient;
}

export function registerDepotRoutes(
  app: FastifyInstance,
  upstreams: DepotUpstreams,
  enabledSources: DepotPackageSourceName[],
  cache: FileCache,
  authHook: preHandlerHookHandler,
  cooldown: ProviderCooldown,
): void {
  // Map each configured source name to its fetch function, preserving the configured order. Sources
  // that aren't enabled simply aren't in this list.
  const factories: Record<DepotPackageSourceName, PackageSource["fetch"]> = {
    ryu: (appid) => upstreams.ryu.downloadDepotPackage(appid),
    depotbox: (appid) => upstreams.depotbox.downloadDepotPackage(appid),
    steamtools: (appid) => upstreams.steamtools.downloadManifestZip(appid),
    hubcap: (appid) => upstreams.hubcap.downloadDepotPackage(appid),
  };
  const allowances: Partial<Record<DepotPackageSourceName, () => Promise<boolean>>> = {
    hubcap: () => upstreams.hubcap.hasAllowance(),
  };
  // Day-limited sources go to the back whatever order they were configured in.
  const ordered = orderedSources(enabledSources);
  const sources: PackageSource[] = ordered.map((name) => ({
    name,
    fetch: factories[name],
    lastResort: isLastResort(name),
    allowed: allowances[name] ?? (async () => true),
  }));
  const normalSources = sources.filter((source) => !source.lastResort);
  const lastResortSources = sources.filter((source) => source.lastResort);
  app.log.info(
    { sources: ordered, lastResort: lastResortSources.map((source) => source.name) },
    "Depot package sources enabled.",
  );

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
        cacheState: "HIT" | "MISS" | "KEPT",
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

      // Tries the given upstreams in order, returning the first stream that opens.
      const openFrom = async (candidates: PackageSource[]): Promise<Readable | null> => {
        for (const candidate of candidates) {
          if (cooldown.isCooling(candidate.name)) {
            lastError ??= new UpstreamError(429, `${candidate.name} is rate limited`);
            continue;
          }
          if (!(await candidate.allowed())) {
            req.log.info({ appid, source: candidate.name }, "Depot source has no allowance left; skipping it.");
            lastError ??= new UpstreamError(429, `${candidate.name} has no allowance left`);
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
            source = candidate.name;
            return stream;
          } catch (error) {
            cooldown.noteFailure(candidate.name, error);
            lastError = error;
            req.log.info({ appid, source: candidate.name, err: error }, "Depot source failed; trying next.");
          }
        }
        return null;
      };

      // The order the package is looked for in, and why:
      //
      //   1. the ordinary providers — a fresh package from one of them replaces whatever was kept,
      //      which is what puts this app back on normal 24h caching;
      //   2. any copy already on disk, however old, before spending a day-limited request on a file
      //      the proxy already has;
      //   3. the day-limited providers (Hubcap), whose answer is *kept*: it stands in for this app
      //      until step 1 succeeds again.
      const openUpstream = async (): Promise<{ stream: Readable; kept?: boolean }> => {
        const normal = await openFrom(normalSources);
        if (normal) return { stream: normal };
        if (await cache.getStale(cacheKey)) {
          // Handled by the caller, which serves that copy rather than spending an allowance on a
          // file the proxy already has.
          throw lastError ?? new UpstreamError(502, `no current depot source for ${appid}`);
        }
        const lastResort = await openFrom(lastResortSources);
        if (lastResort) return { stream: lastResort, kept: true };
        throw lastError ?? new UpstreamError(502, `no depot source served ${appid}`);
      };

      try {
        const { hit, cached } = await cache.getOrFetch(cacheKey, "application/zip", openUpstream);
        const state = cached ? (hit.kept ? "KEPT" : "HIT") : hit.kept ? "KEPT" : "MISS";
        return sendZip(hit.stream(), hit.contentLength, cached ? "cache" : source, state);
      } catch (error) {
        // Every provider failed. A copy on disk — an expired one, or one kept from a day-limited
        // source — is a better answer than an error, and it costs nobody anything.
        const stale = await cache.getStale(cacheKey);
        if (stale) {
          req.log.info(
            { appid, kept: stale.kept, storedAt: stale.storedAt },
            "No provider served the package; serving the copy on disk.",
          );
          return sendZip(stale.stream(), stale.contentLength, "cache", stale.kept ? "KEPT" : "HIT");
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
