// Combines the two upstreams (steamtools + DepotBox) so they fill each other's gaps. Which one is
// primary is decided by the caller (server.ts) via constructor order; the secondary only supplies
// what the primary is missing.
//
//   * MergedGamelistSource  — unions both catalogs by AppID (the primary's entry wins; the secondary
//     adds any AppID the primary does not list). Tolerant: if one upstream is down, the other still
//     serves.
//   * MergedLuaSource       — tries the primary first; if it errors or returns something that is not
//     a real Lua unlock, it falls back to the secondary.

import type { FastifyBaseLogger } from "fastify";
import type { GamelistSource, LuaSource } from "./depotbox.js";
import { UpstreamError, type LuaResult } from "./upstream.js";

export interface RawGame {
  appid?: unknown;
  name?: unknown;
  tags?: unknown;
}

function toAppId(value: unknown): number | null {
  const appid = typeof value === "number" ? value : Number.parseInt(String(value), 10);
  return Number.isInteger(appid) && appid > 0 ? appid : null;
}

function parseGames(raw: string): RawGame[] {
  try {
    const parsed = JSON.parse(raw) as { games?: unknown };
    return Array.isArray(parsed.games) ? (parsed.games as RawGame[]) : [];
  } catch {
    return [];
  }
}

// The proxy client verifies Lua on its own, but the proxy only knows a payload is a real unlock by
// its content — so mirror the client's check (crates/drydock-core/src/proxy.rs `looks_like_lua`) to
// decide whether the primary actually had the game before falling back.
function looksLikeLua(body: string): boolean {
  const head = body.slice(0, 256);
  return head.includes("addappid") || head.includes("setManifestid");
}

// A named provider source in priority order — index 0 wins on shared AppIDs (gamelist) and is tried
// first (lua). The active list is built in server.ts from the global PROVIDER_SOURCES toggle, so a
// deactivated provider (e.g. steamtools/depotbox) simply isn't in the list and never contributes.
export interface NamedGamelistSource {
  name: string;
  source: GamelistSource;
}
export interface NamedLuaSource {
  name: string;
  source: LuaSource;
}

export class MergedGamelistSource implements GamelistSource {
  constructor(
    private readonly sources: NamedGamelistSource[],
    private readonly log: FastifyBaseLogger,
  ) {}

  /**
   * Merges the active providers' catalogs into one array of raw entries, highest priority wins.
   *
   * Providers are fetched and parsed **one at a time**, and each upstream's string and parsed graph is
   * dropped before the next is fetched. `Promise.allSettled` over all of them used to hold three
   * ~35 MB payloads plus three parsed object graphs live simultaneously, which on a small container
   * was the difference between a refresh and an OOM kill.
   *
   * Lowest priority is processed first so that index 0 overwrites last.
   */
  async fetchMerged(): Promise<RawGame[]> {
    if (this.sources.length === 0) {
      throw new UpstreamError(502, "No gamelist sources are enabled (PROVIDER_SOURCES).");
    }
    const byId = new Map<number, RawGame>();
    const counts: Record<string, number> = {};
    let failures = 0;
    let firstError: unknown = null;

    for (const named of [...this.sources].reverse()) {
      try {
        // Scoped so the raw string is unreachable — and collectable — before the next fetch starts.
        let count = 0;
        {
          const raw = await named.source.fetchGamelist();
          for (const game of parseGames(raw)) {
            const appid = toAppId(game.appid);
            if (!appid) continue;
            byId.set(appid, game);
            count += 1;
          }
        }
        counts[named.name] = count;
      } catch (error) {
        failures += 1;
        firstError ??= error;
        this.log.warn({ err: error, source: named.name }, `${named.name} gamelist failed; skipping it.`);
      }
    }

    if (failures === this.sources.length) {
      // Every upstream failed: surface the error so the cache keeps its persisted copy.
      throw firstError instanceof Error
        ? firstError
        : new UpstreamError(502, "All gamelist upstreams failed.");
    }

    this.log.info({ sources: counts, total: byId.size }, "Merged gamelist from active providers.");
    return [...byId.values()];
  }

  /**
   * String form of {@link fetchMerged}, kept for the `GamelistSource` contract.
   *
   * Prefer `fetchMerged`: the cache's compaction step parses whatever this returns, so going through
   * a string means serialising ~100 MB of JSON only to immediately parse it again.
   */
  async fetchGamelist(): Promise<string> {
    return JSON.stringify({ games: await this.fetchMerged() });
  }
}

export class MergedLuaSource implements LuaSource {
  constructor(
    private readonly sources: NamedLuaSource[],
    private readonly log: FastifyBaseLogger,
  ) {}

  async fetchLua(appid: string): Promise<LuaResult> {
    if (this.sources.length === 0) {
      throw new UpstreamError(404, `No lua sources are enabled for AppID ${appid}.`);
    }
    let lastError: unknown = null;
    for (const [index, { name, source }] of this.sources.entries()) {
      try {
        const result = await source.fetchLua(appid);
        if (looksLikeLua(result.body)) {
          if (index > 0) this.log.info({ appid, source: name }, `Lua filled from ${name} (fallback).`);
          return result;
        }
        // Answered, but not a real unlock (this provider does not have the game): try the next.
        lastError = new UpstreamError(404, `${name} has no Lua for AppID ${appid}.`);
      } catch (error) {
        // Prefer a non-404 upstream error over a plain "not found" from an earlier source.
        const haveReal = lastError instanceof UpstreamError && lastError.status !== 404;
        if (!haveReal) lastError = error;
      }
    }
    if (lastError instanceof UpstreamError) throw lastError;
    throw new UpstreamError(404, `No usable Lua for AppID ${appid} from any upstream.`);
  }
}
