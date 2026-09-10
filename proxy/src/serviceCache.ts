// Builds and briefly caches the "Steam Service" manifest (OST DLLs) from the GitHub Contents
// API. The manifest lists every deliverable file with its git-blob sha plus the service
// version, so the app keeps its existing sha verification and rollback and only changes where
// it downloads from. File bytes are streamed fresh from GitHub on each request (routes/service.ts).

import type { FastifyBaseLogger } from "fastify";
import type { Config } from "./config.js";
import type { GitHubClient } from "./github.js";
import { GitHubError } from "./github.js";
import { parseServiceVersion } from "./version.js";

export interface ServiceFile {
  name: string; // unique base filename the client requests
  path: string; // full repo path (internal lookup for the download route)
  sha: string; // git-blob sha
  size?: number;
}

export interface ServiceManifest {
  version: string;
  files: ServiceFile[];
}

interface CachedManifest {
  manifest: ServiceManifest;
  byName: Map<string, ServiceFile>;
  expiresAt: number;
}

export class ServiceCache {
  private cached: CachedManifest | null = null;
  private inflight: Promise<CachedManifest> | null = null;

  constructor(
    private readonly config: Config,
    private readonly github: GitHubClient,
    private readonly log: FastifyBaseLogger,
  ) {}

  async getManifest(): Promise<ServiceManifest> {
    return (await this.load()).manifest;
  }

  async resolveFile(name: string): Promise<ServiceFile | undefined> {
    return (await this.load()).byName.get(name.toLowerCase());
  }

  private async load(): Promise<CachedManifest> {
    const now = Date.now();
    if (this.cached && this.cached.expiresAt > now) return this.cached;
    if (this.inflight) return this.inflight;

    this.inflight = this.build()
      .then((built) => {
        this.cached = built;
        return built;
      })
      .catch((error) => {
        // On refresh failure keep serving the last good manifest if we still have one.
        if (this.cached) {
          this.log.warn({ err: error }, "Service manifest refresh failed; serving previous copy.");
          return this.cached;
        }
        throw error;
      })
      .finally(() => {
        this.inflight = null;
      });

    return this.inflight;
  }

  private async build(): Promise<CachedManifest> {
    const entries = await this.github.listDirectory(this.config.serviceDirectory);
    const versionPath = this.config.serviceVersionPath;

    const byName = new Map<string, ServiceFile>();
    const files: ServiceFile[] = [];
    for (const entry of entries) {
      if (entry.type === "dir") {
        // The service folder must be flat: a nested subdirectory is a maintenance error.
        throw new GitHubError(502, "The Steam Service folder may only contain files directly in it.");
      }
      if (entry.type !== "file") continue;
      if (entry.path === versionPath || entry.name.toLowerCase() === "version.json") continue;
      const name = entry.name;
      if (!name) continue;
      if (byName.has(name.toLowerCase())) {
        throw new GitHubError(502, `The Steam Service folder contains the duplicate filename ${name}.`);
      }
      const file: ServiceFile = { name, path: entry.path, sha: entry.sha, ...(entry.size !== undefined ? { size: entry.size } : {}) };
      byName.set(name.toLowerCase(), file);
      files.push(file);
    }

    if (files.length === 0) throw new GitHubError(502, "The Steam Service folder contains no installable files.");
    files.sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));

    const version = parseServiceVersion(await this.github.fetchRawText(versionPath));

    this.log.info({ count: files.length, version }, "Service manifest built.");
    return {
      manifest: { version, files },
      byName,
      expiresAt: Date.now() + this.config.serviceManifestTtlSeconds * 1000,
    };
  }
}
