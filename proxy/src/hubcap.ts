// Client for Hubcap Manifest (hubcapmanifest.com).
//
// Hubcap is the **last resort** for depot packages: its manifest downloads are capped per day (the
// other providers are capped per minute at worst), so it is only asked once every other provider has
// failed, and what it hands over is kept on the proxy until one of them can serve that app again
// (see `routes/depot.ts` and the pinned entries in `fileCache.ts`).
//
// Its library, on the other hand, costs nothing and is large (~157k apps), so it feeds the gamelist
// like any other provider — at the lowest priority, so it only fills in apps the others do not list.
//
// Endpoints used:
//   GET /api/v1/library?limit=&offset=   -> { total_count, games: [{ game_id, game_name, … }] }  (free)
//   GET /api/v1/manifest/{appid}         -> application/zip (`<appid>.lua` + `<depot>_<gid>.manifest`)
//   GET /api/v1/generate/usage           -> { single: { usage, limit, remaining }, … }           (free)
//
// The daily allowance is reported by `generate/usage` as the `single` bucket. Serving a manifest
// that is already built does not spend it; building one does. The proxy therefore never asks for a
// refresh (`force_update`), and it stops asking Hubcap at all once the bucket is down to its
// reserve — a quota spent on one app is an app the proxy cannot serve for the rest of the day.

import type { Config } from "./config.js";
import type { GamelistSource, RawStream } from "./depotbox.js";
import { UpstreamError, upstreamError } from "./upstream.js";
import { type Deadline, fetchWithDeadline, readBody, streamBody } from "./http.js";

const HUBCAP: Deadline = { label: "Hubcap", error: upstreamError };

/** Stops a library walk that would otherwise never end if the API ignored `offset`. */
const MAXIMUM_LIBRARY_PAGES = 200;

interface LibraryPage {
  total_count?: unknown;
  games?: unknown;
}

interface LibraryEntry {
  game_id?: unknown;
  game_name?: unknown;
  manifest_available?: unknown;
}

interface UsageBucket {
  usage?: unknown;
  limit?: unknown;
  remaining?: unknown;
}

/** What is left of the daily allowance, as `generate/usage` reports it. */
export interface HubcapQuota {
  remaining: number;
  limit: number;
}

function toAppId(value: unknown): number | null {
  const appid = typeof value === "number" ? value : Number.parseInt(String(value ?? ""), 10);
  return Number.isInteger(appid) && appid > 0 ? appid : null;
}

function toCount(value: unknown): number | null {
  const count = typeof value === "number" ? value : Number.parseInt(String(value ?? ""), 10);
  return Number.isFinite(count) ? count : null;
}

export class HubcapClient implements GamelistSource {
  /** The last quota reading and when it was taken, so the free endpoint is not polled per request. */
  private quota: { value: HubcapQuota; at: number } | null = null;

  constructor(private readonly config: Config) {}

  private headers(): Record<string, string> {
    return {
      authorization: `Bearer ${this.config.hubcapApiKey}`,
      "user-agent": "drydock-proxy/1.0",
      accept: "*/*",
    };
  }

  private requireKey(): void {
    if (!this.config.hubcapApiKey) {
      throw new UpstreamError(502, "Hubcap API key is not configured (set HUBCAP_API_KEY).");
    }
  }

  /**
   * The whole library as the `{ games: [{ appid, name, tags }] }` shape the merged gamelist wants.
   *
   * Walked page by page, keeping only the two fields that survive the merge: a page is several
   * megabytes of JSON, and holding all of them at once is how a small container runs out of memory.
   * Entries without a usable App ID or name, and ones whose manifest is not available, are dropped —
   * listing a game the proxy could never serve only produces a dead end in the client.
   */
  async fetchGamelist(): Promise<string> {
    this.requireKey();
    const pageSize = this.config.hubcapLibraryPageSize;
    const games: { appid: number; name: string; tags: never[] }[] = [];
    const seen = new Set<number>();
    let total: number | null = null;

    for (let page = 0; page < MAXIMUM_LIBRARY_PAGES; page += 1) {
      const offset = page * pageSize;
      const url = `${this.config.hubcapBase}/api/v1/library?limit=${pageSize}&offset=${offset}`;
      const response = await fetchWithDeadline(url, this.headers(), this.config.gamelistTimeoutMs, HUBCAP);
      if (!response.ok) {
        throw new UpstreamError(response.status, `Hubcap library returned ${response.status}.`);
      }
      const body = (await readBody(response.json(), HUBCAP)) as LibraryPage | null;
      const entries = Array.isArray(body?.games) ? (body.games as LibraryEntry[]) : null;
      if (!entries) throw new UpstreamError(502, "Invalid Hubcap library schema.");
      total ??= toCount(body?.total_count);

      for (const entry of entries) {
        const appid = toAppId(entry.game_id);
        const name = String(entry.game_name ?? "").trim();
        if (!appid || name.length === 0 || entry.manifest_available === false) continue;
        // A repeated App ID means the page window shifted under us (the library is sorted by last
        // update); keeping the first is enough, and it also stops a duplicate blowing up the array.
        if (seen.has(appid)) continue;
        seen.add(appid);
        games.push({ appid, name, tags: [] });
      }

      const done = entries.length < pageSize || (total !== null && offset + entries.length >= total);
      if (done) break;
    }

    if (games.length === 0) throw new UpstreamError(502, "Hubcap library returned no usable games.");
    return JSON.stringify({ games });
  }

  /**
   * What is left of today's allowance, or `null` when it cannot be read.
   *
   * The reading is cached for `HUBCAP_USAGE_TTL_SECONDS`: it is only consulted before spending a
   * request, and a stale-by-minutes number is better than an extra round trip in front of a download
   * that is already the slow path.
   */
  async remaining(): Promise<HubcapQuota | null> {
    this.requireKey();
    const ttl = this.config.hubcapUsageTtlSeconds * 1000;
    if (this.quota && Date.now() - this.quota.at < ttl) return this.quota.value;

    const url = `${this.config.hubcapBase}/api/v1/generate/usage`;
    const response = await fetchWithDeadline(url, this.headers(), this.config.upstreamTimeoutMs, HUBCAP);
    if (!response.ok) {
      throw new UpstreamError(response.status, `Hubcap usage returned ${response.status}.`);
    }
    const body = (await readBody(response.json(), HUBCAP)) as { single?: UsageBucket } | null;
    const remaining = toCount(body?.single?.remaining);
    const limit = toCount(body?.single?.limit);
    if (remaining === null) return null;
    const value: HubcapQuota = { remaining, limit: limit ?? 0 };
    this.quota = { value, at: Date.now() };
    return value;
  }

  /**
   * Whether Hubcap may be asked for a package right now: `false` when the daily allowance is down to
   * the configured reserve. A quota that cannot be read does not block the request — the download
   * itself still answers 429 when there is nothing left, and that is handled like any other provider.
   */
  async hasAllowance(): Promise<boolean> {
    try {
      const quota = await this.remaining();
      return quota === null || quota.remaining > this.config.hubcapReserve;
    } catch {
      return true;
    }
  }

  /** Opens a streaming download of the app's manifest ZIP (`<appid>.lua` + `.manifest` files). */
  async downloadDepotPackage(appid: string): Promise<RawStream> {
    this.requireKey();
    // No `force_update`: a rebuild is what actually spends the daily allowance, and the proxy only
    // needs the manifest that exists.
    const url = `${this.config.hubcapBase}/api/v1/manifest/${encodeURIComponent(appid)}`;
    const response = await fetchWithDeadline(
      url,
      this.headers(),
      this.config.depotPackageTimeoutMs,
      HUBCAP,
      true,
    );
    if (!response.ok || !response.body) {
      throw new UpstreamError(
        response.ok ? 502 : response.status,
        `Hubcap manifest returned ${response.status} for AppID ${appid}.`,
      );
    }
    // A served download changes what is left, so the cached reading is dropped rather than trusted
    // until its TTL runs out.
    this.quota = null;
    const lengthHeader = response.headers.get("content-length");
    return {
      stream: streamBody(response.body, HUBCAP),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/zip",
    };
  }
}
