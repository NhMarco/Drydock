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
  const parsed = JSON.parse(raw) as { games?: unknown; success?: unknown } | null;
  if (!parsed || parsed.success === false || !Array.isArray(parsed.games)) {
    throw new UpstreamError(502, "Invalid gamelist schema.");
  }
  return parsed.games as RawGame[];
}

function hasTags(game: RawGame): boolean {
  return Array.isArray(game.tags) && game.tags.length > 0;
}

// The proxy client verifies Lua on its own, but the proxy only knows a payload is a real unlock by
// its content — so mirror the client's check (crates/drydock-core/src/proxy.rs `looks_like_lua`) to
// decide whether a provider actually had the game before falling back. A line has to start with the
// call, commented out or not. The whole body is searched: providers put a header of comments first,
// and an HTML or JSON error page is never a Lua.
const UNLOCK_LINE = /^\s*(?:--\s*)?(addappid|setManifestid)\s*\(/m;
export function looksLikeLua(body: string): boolean {
  const start = body.trimStart();
  return !start.startsWith("<") && !start.startsWith("{") && UNLOCK_LINE.test(body);
}

// Providers that answered 429, skipped until their limit resets so one busy provider does not turn
// every request into a slow failure. Shared by the Lua and depot routes.
export class ProviderCooldown {
  private readonly until = new Map<string, number>();

  isCooling(name: string): boolean {
    const until = this.until.get(name);
    if (until === undefined) return false;
    if (until > Date.now()) return true;
    this.until.delete(name);
    return false;
  }

  noteFailure(name: string, error: unknown): void {
    if (error instanceof UpstreamError && error.status === 429) {
      this.until.set(name, Date.now() + (error.retryAfterSeconds ?? 60) * 1000);
    }
  }
}

// Keeps the more telling of two failures: a real upstream error outranks a plain "not found".
function moreTelling(current: unknown, next: unknown): unknown {
  return current instanceof UpstreamError && current.status !== 404 ? current : next;
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
            // A provider without tags (DepotBox) keeps the tags a lower-priority one had, so the
            // NSFW filter still sees them.
            const previous = byId.get(appid);
            byId.set(appid, previous && hasTags(previous) && !hasTags(game) ? { ...game, tags: previous.tags } : game);
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

    if (byId.size === 0) throw new UpstreamError(502, "No usable games returned by active providers.");
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
    private readonly cooldown: ProviderCooldown = new ProviderCooldown(),
  ) {}

  async fetchLua(appid: string): Promise<LuaResult> {
    if (this.sources.length === 0) {
      throw new UpstreamError(404, `No lua sources are enabled for AppID ${appid}.`);
    }
    let lastError: unknown = null;
    for (const [index, { name, source }] of this.sources.entries()) {
      if (this.cooldown.isCooling(name)) {
        lastError = moreTelling(lastError, new UpstreamError(429, `${name} is rate limited.`));
        continue;
      }
      try {
        const result = await source.fetchLua(appid);
        if (looksLikeLua(result.body)) {
          if (index > 0) this.log.info({ appid, source: name }, `Lua filled from ${name} (fallback).`);
          return result;
        }
        // Answered, but not a real unlock (this provider does not have the game): try the next.
        lastError = moreTelling(lastError, new UpstreamError(404, `${name} has no Lua for AppID ${appid}.`));
      } catch (error) {
        this.cooldown.noteFailure(name, error);
        // An expired key or an exhausted quota would otherwise only show up as a missing Lua.
        const status = error instanceof UpstreamError ? error.status : undefined;
        if (status !== 404) this.log.warn({ appid, source: name, upstreamStatus: status, err: error }, `${name} Lua failed.`);
        lastError = moreTelling(lastError, error);
      }
    }
    if (lastError instanceof UpstreamError) throw lastError;
    throw new UpstreamError(404, `No usable Lua for AppID ${appid} from any upstream.`);
  }
}
