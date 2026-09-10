// A small disk-backed cache for user-facing upstream files (depot packages, magicfiles, emu
// binaries, …). Each entry is stored under `dataDir/filecache/<sha1(key)>` with a `.json` sidecar
// holding its content type and store time. Entries older than the TTL (24h) are treated as misses
// and refetched, so upstream APIs (Ryu/DepotBox/SteamTools/GitHub) are hit at most once per file
// per day. Values can be large (depot ZIPs), so nothing is held in memory — writes stream to a
// temp file and are atomically renamed into place.

import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, readdir, rename, rm, stat, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pipeline } from "node:stream/promises";
import type { Readable } from "node:stream";
import type { FastifyBaseLogger } from "fastify";

interface Meta {
  contentType: string;
  storedAt: number;
  size: number;
}

export interface CacheHit {
  stream: () => Readable;
  contentType: string;
  contentLength: number;
}

// Expired entries are deleted once they are this much older than the TTL. The grace period means a
// key that is still being requested regularly is refreshed in place rather than deleted and refetched.
const SWEEP_GRACE_FACTOR = 2;

// How often the background sweep runs once started.
const SWEEP_INTERVAL_MS = 60 * 60 * 1000;

export class FileCache {
  private readonly dir: string;
  private sweepTimer: NodeJS.Timeout | null = null;
  /** In-flight `put`s by key, so concurrent misses share one upstream fetch. */
  private readonly inFlight = new Map<string, Promise<CacheHit>>();

  constructor(
    dataDir: string,
    private readonly ttlMs: number,
    private readonly log: FastifyBaseLogger,
  ) {
    this.dir = join(dataDir, "filecache");
  }

  private paths(key: string): { data: string; meta: string } {
    const hash = createHash("sha1").update(key).digest("hex");
    return { data: join(this.dir, hash), meta: join(this.dir, `${hash}.json`) };
  }

  /**
   * Starts the periodic eviction sweep (and runs one immediately).
   *
   * Without this the cache only ever grew: an entry past its TTL counted as a miss but its file was
   * never removed, so every depot ZIP a user downloaded once and never asked for again stayed on disk
   * forever — a slow, guaranteed march to a full volume.
   */
  start(): void {
    if (this.sweepTimer) return;
    void this.sweep();
    this.sweepTimer = setInterval(() => void this.sweep(), SWEEP_INTERVAL_MS);
    this.sweepTimer.unref();
  }

  stop(): void {
    if (this.sweepTimer) clearInterval(this.sweepTimer);
    this.sweepTimer = null;
  }

  /** Deletes entries whose metadata says they expired more than the grace period ago. */
  async sweep(): Promise<{ removed: number; freedBytes: number }> {
    if (this.ttlMs <= 0) return { removed: 0, freedBytes: 0 };
    let removed = 0;
    let freedBytes = 0;
    const cutoff = Date.now() - this.ttlMs * SWEEP_GRACE_FACTOR;
    let names: string[];
    try {
      names = await readdir(this.dir);
    } catch {
      return { removed: 0, freedBytes: 0 }; // nothing cached yet
    }
    for (const name of names) {
      if (!name.endsWith(".json")) continue; // data files are handled via their sidecar
      const meta = join(this.dir, name);
      const data = meta.slice(0, -".json".length);
      try {
        const parsed = JSON.parse(await readFile(meta, "utf8")) as Meta;
        if (Number.isFinite(parsed.storedAt) && parsed.storedAt > cutoff) continue;
        await rm(data, { force: true });
        await rm(meta, { force: true });
        removed += 1;
        freedBytes += Number.isFinite(parsed.size) ? parsed.size : 0;
      } catch {
        // Unreadable sidecar: drop both halves, the entry is unusable anyway.
        await rm(data, { force: true }).catch(() => {});
        await rm(meta, { force: true }).catch(() => {});
        removed += 1;
      }
    }
    // Orphaned temp files from a crashed write.
    for (const name of names) {
      if (!name.endsWith(".tmp")) continue;
      await rm(join(this.dir, name), { force: true }).catch(() => {});
    }
    if (removed > 0) {
      this.log.info({ removed, freedBytes }, "File cache sweep removed expired entries.");
    }
    return { removed, freedBytes };
  }

  /** A fresh cached entry for `key`, or `null` when absent or older than the TTL. */
  async get(key: string): Promise<CacheHit | null> {
    if (this.ttlMs <= 0) return null;
    const { data, meta } = this.paths(key);
    try {
      const parsed = JSON.parse(await readFile(meta, "utf8")) as Meta;
      if (!Number.isFinite(parsed.storedAt) || Date.now() - parsed.storedAt > this.ttlMs) return null;
      const info = await stat(data);
      if (info.size !== parsed.size) return null;
      return {
        stream: () => createReadStream(data),
        contentType: parsed.contentType,
        contentLength: parsed.size,
      };
    } catch {
      return null;
    }
  }

  /** Stores `source` under `key`, returning the resulting cache hit so the caller can serve it. */
  async put(key: string, source: Readable, contentType: string): Promise<CacheHit> {
    const { data, meta } = this.paths(key);
    await mkdir(this.dir, { recursive: true });
    const temporary = `${data}.${process.pid}.${Date.now()}.${Math.random().toString(36).slice(2)}.tmp`;
    try {
      await pipeline(source, createWriteStream(temporary));
      const info = await stat(temporary);
      // Write the sidecar *before* publishing the data file, so a reader can never see a fresh data
      // file described by the previous entry's metadata (which `get` would then reject on the size
      // check, making the key permanently un-cacheable).
      const record: Meta = { contentType, storedAt: Date.now(), size: info.size };
      await writeFile(`${meta}.tmp`, JSON.stringify(record));
      await rename(temporary, data);
      await rename(`${meta}.tmp`, meta);
      return { stream: () => createReadStream(data), contentType, contentLength: info.size };
    } catch (error) {
      await rm(temporary, { force: true }).catch(() => {});
      await rm(`${meta}.tmp`, { force: true }).catch(() => {});
      this.log.warn({ err: error, key }, "File cache write failed.");
      throw error;
    }
  }

  /**
   * Returns the cached entry for `key`, fetching it through `open` on a miss — with only **one**
   * upstream fetch in flight per key.
   *
   * Without this each concurrent request for the same uncached app triggered its own upstream
   * packaging job (minutes of work for a large title) and they all raced to write the same cache
   * entry. Followers here await the leader's result and then serve it from disk.
   */
  async getOrFetch(
    key: string,
    contentType: string,
    open: () => Promise<Readable>,
  ): Promise<{ hit: CacheHit; cached: boolean }> {
    const existing = await this.get(key);
    if (existing) return { hit: existing, cached: true };

    const pending = this.inFlight.get(key);
    if (pending) return { hit: await pending, cached: true };

    const work = (async () => {
      const source = await open();
      return this.put(key, source, contentType);
    })();
    this.inFlight.set(key, work);
    try {
      return { hit: await work, cached: false };
    } finally {
      this.inFlight.delete(key);
    }
  }
}
