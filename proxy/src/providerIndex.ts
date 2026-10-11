// Which apps each provider lists, and — where its list says so — when it last updated an app's
// unlock. The app shows this when the user picks a source for "Add to Steam" or a download
// (`GET /v1/sources/:appid`). It is built from the gamelists the proxy fetches anyway, held as sorted
// typed arrays (a few MB for every provider together, not a map per app), and persisted beside the
// gamelist so a restart has it at once instead of after the next hourly refresh.

import { readFile, rename, writeFile } from "node:fs/promises";
import type { FastifyBaseLogger } from "fastify";

/** Providers whose gamelist names every Steam app, so being on it says nothing about an unlock. */
export const LISTS_EVERY_APP: ReadonlySet<string> = new Set(["steamtools"]);

/** The gamelist field in which a provider says when it last updated an app's unlock. */
const UPDATED_FIELD: Readonly<Record<string, string>> = { ryu: "updated_date" };

export interface ProviderListing {
  /** The App IDs the provider lists, ascending. */
  ids: Uint32Array;
  /** Parallel to `ids`: when each unlock was last updated, in Unix seconds (0 = not said). */
  updated: Uint32Array | null;
}

/** What one provider says about one app. */
export interface ProviderAnswer {
  /** Listed by the provider; `null` when its list cannot tell (it lists every Steam app). */
  listed: boolean | null;
  /** When it last updated the app's unlock, in Unix seconds, where its list says so. */
  updated: number | null;
}

function toAppId(value: unknown): number | null {
  const appid = typeof value === "number" ? value : Number.parseInt(String(value), 10);
  return Number.isInteger(appid) && appid > 0 && appid <= 0xffff_ffff ? appid : null;
}

function toSeconds(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value) && value > 0) {
    return Math.floor(value > 1e12 ? value / 1000 : value);
  }
  if (typeof value !== "string" || value === "") return 0;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed / 1000) : 0;
}

/** A provider's listing from its parsed gamelist entries; `null` for a provider not worth indexing. */
export function buildListing(name: string, games: readonly object[]): ProviderListing | null {
  if (LISTS_EVERY_APP.has(name)) return null;
  const field = UPDATED_FIELD[name];
  const rows: [number, number][] = [];
  for (const game of games) {
    const entry = game as Record<string, unknown>;
    const appid = toAppId(entry.appid);
    if (appid !== null) rows.push([appid, field ? toSeconds(entry[field]) : 0]);
  }
  rows.sort((a, b) => a[0] - b[0]);
  const ids = new Uint32Array(rows.length);
  const updated = field ? new Uint32Array(rows.length) : null;
  rows.forEach(([appid, seconds], index) => {
    ids[index] = appid;
    if (updated) updated[index] = seconds;
  });
  return { ids, updated };
}

function find(ids: Uint32Array, appid: number): number {
  let low = 0;
  let high = ids.length - 1;
  while (low <= high) {
    const middle = (low + high) >>> 1;
    const value = ids[middle]!;
    if (value === appid) return middle;
    if (value < appid) low = middle + 1;
    else high = middle - 1;
  }
  return -1;
}

const toBase64 = (array: Uint32Array) => Buffer.from(array.buffer, array.byteOffset, array.byteLength).toString("base64");
const fromBase64 = (text: string) => {
  const bytes = Buffer.from(text, "base64");
  return new Uint32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
};

export class ProviderIndex {
  private readonly listings = new Map<string, ProviderListing>();

  constructor(
    private readonly file: string | null,
    private readonly log: FastifyBaseLogger,
  ) {}

  /** Replaces one provider's listing (a provider whose list failed keeps its previous one). */
  set(name: string, listing: ProviderListing): void {
    this.listings.set(name, listing);
  }

  /** What `name` says about `appid`, or `null` when nothing is known about that provider yet. */
  lookup(name: string, appid: number): ProviderAnswer | null {
    if (LISTS_EVERY_APP.has(name)) return { listed: null, updated: null };
    const listing = this.listings.get(name);
    if (!listing) return null;
    const index = find(listing.ids, appid);
    if (index < 0) return { listed: false, updated: null };
    const seconds = listing.updated?.[index] ?? 0;
    return { listed: true, updated: seconds > 0 ? seconds : null };
  }

  async load(): Promise<void> {
    if (!this.file) return;
    try {
      const stored = JSON.parse(await readFile(this.file, "utf8")) as Record<string, { ids: string; updated: string | null }>;
      for (const [name, listing] of Object.entries(stored)) {
        const ids = fromBase64(listing.ids);
        const updated = listing.updated === null ? null : fromBase64(listing.updated);
        if (updated && updated.length !== ids.length) continue;
        this.listings.set(name, { ids, updated });
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
        this.log.warn({ err: error }, "Stored provider index unreadable; it is rebuilt with the next gamelist.");
      }
    }
  }

  async save(): Promise<void> {
    if (!this.file) return;
    const body: Record<string, { ids: string; updated: string | null }> = {};
    for (const [name, listing] of this.listings) {
      body[name] = { ids: toBase64(listing.ids), updated: listing.updated ? toBase64(listing.updated) : null };
    }
    const temporary = `${this.file}.${process.pid}.tmp`;
    try {
      await writeFile(temporary, JSON.stringify(body));
      await rename(temporary, this.file);
    } catch (error) {
      this.log.warn({ err: error }, "Could not persist the provider index.");
    }
  }
}
