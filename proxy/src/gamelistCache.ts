// Holds the steamtools gamelist so individual clients never trigger an upstream fetch.
// The full upstream payload is ~35 MB of uncompressed JSON with fields the app does not need.
// We compact it to { appid, name, tags }, gzip it once, and serve that buffer to everyone,
// refreshing on a fixed interval. The compacted+gzipped copy is persisted to a mounted
// volume so a restart serves data immediately and a temporary upstream outage is survivable.

import { createHash } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { gzip, gunzip } from "node:zlib";
import { promisify } from "node:util";
import type { FastifyBaseLogger } from "fastify";
import type { Config } from "./config.js";
import type { GamelistSource } from "./depotbox.js";
import type { RawGame } from "./merged.js";

/** A gamelist source that can hand over already-parsed entries, skipping a JSON round trip. */
interface MergedGamelistCapable extends GamelistSource {
  fetchMerged?: () => Promise<RawGame[]>;
}

const gzipAsync = promisify(gzip);
const gunzipAsync = promisify(gunzip);

// Until the very first snapshot lands, retry this often (e.g. upstream briefly down at boot)
// instead of waiting a full refresh interval; afterwards the normal hourly cadence applies.
const BOOT_RETRY_MS = 60_000;

interface CompactApp {
  appid: number;
  name: string;
  tags: string[];
}

interface RawApp {
  appid?: unknown;
  name?: unknown;
  tags?: unknown;
}

export interface GamelistSnapshot {
  gzipBody: Buffer;
  etag: string;
  count: number;
  updatedAt: number; // Unix seconds
}

interface PersistedMeta {
  etag: string;
  count: number;
  updatedAt: number;
}

export class GamelistCache {
  private snapshot: GamelistSnapshot | null = null;
  private refreshing: Promise<void> | null = null;
  private timer: NodeJS.Timeout | null = null;

  private readonly gzipPath: string;
  private readonly metaPath: string;

  constructor(
    private readonly config: Config,
    private readonly client: MergedGamelistCapable,
    private readonly log: FastifyBaseLogger,
  ) {
    this.gzipPath = join(config.dataDir, "gamelist.json.gz");
    this.metaPath = join(config.dataDir, "gamelist.meta.json");
  }

  get current(): GamelistSnapshot | null {
    return this.snapshot;
  }

  // Loads any persisted copy, then guarantees a usable snapshot before returning, then
  // schedules periodic refreshes. Never throws: if the very first fetch fails and no
  // persisted copy exists, the gamelist route reports 503 until a later refresh succeeds.
  async start(): Promise<void> {
    await mkdir(this.config.dataDir, { recursive: true });
    await this.loadFromDisk();

    if (!this.snapshot) {
      await this.refresh().catch((error) => {
        this.log.error({ err: error }, "Initial gamelist fetch failed; will retry on the refresh interval.");
      });
    } else {
      // Refresh in the background so boot is fast even with a stale-but-usable copy.
      void this.refresh().catch((error) => {
        this.log.warn({ err: error }, "Background gamelist refresh after boot failed; serving persisted copy.");
      });
    }

    this.scheduleNext();
  }

  stop(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  // Self-reschedules: fast retry while there is still no snapshot, hourly once one exists.
  private scheduleNext(): void {
    const delayMs = this.snapshot ? this.config.gamelistRefreshMinutes * 60 * 1000 : BOOT_RETRY_MS;
    this.timer = setTimeout(() => {
      void this.refresh()
        .catch((error) => this.log.warn({ err: error }, "Scheduled gamelist refresh failed; will retry."))
        .finally(() => this.scheduleNext());
    }, delayMs);
    this.timer.unref();
  }

  // Coalesces concurrent refreshes so an interval tick during a slow fetch cannot pile up.
  refresh(): Promise<void> {
    if (this.refreshing) return this.refreshing;
    this.refreshing = this.doRefresh().finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  private async doRefresh(): Promise<void> {
    const startedAt = Date.now();
    // Take the merged entries directly when the source can provide them. Going through a JSON string
    // meant the merge serialised ~100 MB only for `compactGamelist` to parse it straight back — a
    // second full parse of the same data, blocking the event loop for seconds on every refresh.
    const fetchMerged = this.client.fetchMerged?.bind(this.client);
    const compact = fetchMerged
      ? compactEntries(await fetchMerged(), this.config.filterNsfw)
      : compactGamelist(await this.client.fetchGamelist(), this.config.filterNsfw);
    const json = Buffer.from(JSON.stringify({ games: compact }), "utf8");
    const gzipBody = await gzipAsync(json, { level: 6 });
    const etag = `"${createHash("sha256").update(gzipBody).digest("hex").slice(0, 32)}"`;

    this.snapshot = {
      gzipBody,
      etag,
      count: compact.length,
      updatedAt: Math.floor(Date.now() / 1000),
    };
    await this.persist(this.snapshot);
    this.log.info(
      { count: compact.length, gzipBytes: gzipBody.length, ms: Date.now() - startedAt },
      "Gamelist refreshed.",
    );
  }

  private async persist(snapshot: GamelistSnapshot): Promise<void> {
    try {
      // Write to temp files then rename so a crash mid-write never leaves a partial cache.
      const gzipTmp = `${this.gzipPath}.${process.pid}.tmp`;
      const metaTmp = `${this.metaPath}.${process.pid}.tmp`;
      const meta: PersistedMeta = { etag: snapshot.etag, count: snapshot.count, updatedAt: snapshot.updatedAt };
      await writeFile(gzipTmp, snapshot.gzipBody);
      await writeFile(metaTmp, JSON.stringify(meta), "utf8");
      await rename(gzipTmp, this.gzipPath);
      await rename(metaTmp, this.metaPath);
    } catch (error) {
      this.log.warn({ err: error }, "Could not persist gamelist to disk; serving from memory only.");
    }
  }

  private async loadFromDisk(): Promise<void> {
    if (!existsSync(this.gzipPath) || !existsSync(this.metaPath)) return;
    try {
      const [gzipBody, metaRaw] = await Promise.all([readFile(this.gzipPath), readFile(this.metaPath, "utf8")]);
      const meta = JSON.parse(metaRaw) as PersistedMeta;
      // Sanity-check the persisted body actually decompresses before trusting it.
      await gunzipAsync(gzipBody);
      this.snapshot = {
        gzipBody,
        etag: meta.etag,
        count: meta.count,
        updatedAt: meta.updatedAt,
      };
      this.log.info({ count: meta.count, updatedAt: meta.updatedAt }, "Loaded persisted gamelist from disk.");
    } catch (error) {
      this.log.warn({ err: error }, "Persisted gamelist was unreadable; ignoring it.");
      this.snapshot = null;
    }
  }
}

// Parses the upstream JSON and keeps only the fields the app needs. Prefer `compactEntries` when the
// caller already holds parsed objects — this wrapper exists for sources that can only produce a string.
export function compactGamelist(raw: string, filterNsfw: boolean): CompactApp[] {
  const parsed = JSON.parse(raw) as { games?: unknown };
  const games = parsed.games;
  if (!Array.isArray(games)) throw new Error("Upstream gamelist did not contain a 'games' array.");
  return compactEntries(games as RawApp[], filterNsfw);
}

// Keeps only the fields the app needs. Upstream tags are prefixed (`tag:action`, `nsfw:gore`); we
// strip the `tag:` prefix for clean genre tags, drop the `nsfw:` markers, and — when `filterNsfw` is
// set — omit any entry that carried an NSFW marker. Anything malformed is skipped rather than failing
// the whole refresh.
export function compactEntries(games: RawApp[], filterNsfw: boolean): CompactApp[] {
  const result: CompactApp[] = [];
  for (const entry of games) {
    const appid = typeof entry.appid === "number" ? entry.appid : Number.parseInt(String(entry.appid), 10);
    if (!Number.isInteger(appid) || appid <= 0) continue;
    const name = typeof entry.name === "string" ? entry.name : "";
    if (!name) continue;

    const rawTags = Array.isArray(entry.tags) ? (entry.tags.filter((t) => typeof t === "string") as string[]) : [];
    const isNsfw = rawTags.some((t) => t.toLowerCase().startsWith("nsfw:"));
    if (filterNsfw && isNsfw) continue;
    const tags = rawTags
      .filter((t) => !t.toLowerCase().startsWith("nsfw:"))
      .map((t) => (t.toLowerCase().startsWith("tag:") ? t.slice("tag:".length) : t));

    result.push({ appid, name, tags });
  }
  return result;
}
