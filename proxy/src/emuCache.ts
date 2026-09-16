// Lists the emulator DLLs the cracker deploys, from the GitHub Contents API.
//
// Same shape as the Steam Service payload (serviceCache.ts) minus the version file: this is a flat
// directory of binaries, and the client decides which of them it needs for a given architecture.
// Each entry carries its git-blob sha so the client can verify the bytes it downloads, exactly as it
// already does for the service payload.

import type { FastifyBaseLogger } from "fastify";
import type { Config } from "./config.js";
import type { GitHubClient } from "./github.js";

export interface EmuFile {
  /** Unique base filename the client requests. */
  name: string;
  /** Full repo path, kept server-side for the download route. */
  path: string;
  sha: string;
  size?: number;
}

interface CachedListing {
  files: EmuFile[];
  byName: Map<string, EmuFile>;
  expiresAt: number;
}

/** Placeholder files git needs to keep an otherwise-empty directory; never a deliverable. */
const IGNORED_NAMES = new Set(["init.txt", ".gitkeep", ".gitignore", "readme.md"]);

export class EmuCache {
  private cached: CachedListing | null = null;
  private inflight: Promise<CachedListing> | null = null;

  constructor(
    private readonly config: Config,
    private readonly github: GitHubClient,
    private readonly log: FastifyBaseLogger,
  ) {}

  async getFiles(): Promise<EmuFile[]> {
    return (await this.listing()).files;
  }

  async resolveFile(name: string): Promise<EmuFile | undefined> {
    return (await this.listing()).byName.get(name.toLowerCase());
  }

  /** Cached directory listing, with concurrent refreshes collapsed into one upstream call. */
  private async listing(): Promise<CachedListing> {
    if (this.cached && this.cached.expiresAt > Date.now()) return this.cached;
    if (this.inflight) return this.inflight;

    this.inflight = this.build()
      .then((listing) => {
        this.cached = listing;
        return listing;
      })
      .finally(() => {
        this.inflight = null;
      });
    return this.inflight;
  }

  private async build(): Promise<CachedListing> {
    const entries = await this.github.listDirectory(this.config.emuDirectory);
    const files: EmuFile[] = [];
    for (const entry of entries) {
      if (entry.type !== "file") continue;
      if (IGNORED_NAMES.has(entry.name.toLowerCase())) continue;
      files.push({
        name: entry.name,
        path: entry.path,
        sha: entry.sha,
        ...(entry.size !== undefined ? { size: entry.size } : {}),
      });
    }
    files.sort((a, b) => a.name.localeCompare(b.name));
    this.log.info({ count: files.length, directory: this.config.emuDirectory }, "Emulator file listing refreshed.");
    return {
      files,
      byName: new Map(files.map((file) => [file.name.toLowerCase(), file])),
      expiresAt: Date.now() + this.config.emuListingTtlSeconds * 1000,
    };
  }
}
