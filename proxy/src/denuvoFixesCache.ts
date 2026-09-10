// Builds and briefly caches the per-app "Denuvo fix" manifest from the GitHub Contents API
// (`Files/fix`). A Denuvo fix is offered only when both halves are present:
//   * `Files/fix/{appid}.lua`  — the build-locked unlock Lua (replaces the normal token Lua)
//   * `Files/fix/{appid}.zip`  — the game-folder payload, either a single zip or ordered raw
//     byte-split parts `{appid}.zip.001`, `{appid}.zip.002`, … (GitHub caps files at 100 MB).
// File bytes are streamed fresh from GitHub on request (routes/denuvoFixes.ts); only the small
// manifest (names + git-blob shas) is cached. This is the only fix source now (the DepotBox
// "online fix" API path was removed).

import type { FastifyBaseLogger } from "fastify";
import type { Config } from "./config.js";
import type { ContentEntry, GitHubClient } from "./github.js";

export interface FixFileRef {
  name: string;
  path: string;
  sha: string;
  size?: number;
}

export interface DenuvoFixEntry {
  appid: number;
  lua: FixFileRef;
  zip_parts: FixFileRef[];
}

interface CachedFixes {
  fixes: DenuvoFixEntry[];
  byName: Map<string, FixFileRef>; // download whitelist: every servable fix file name
  expiresAt: number;
}

function parseU32(value: string): number | null {
  if (!/^[0-9]+$/.test(value)) return null;
  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed > 0 && parsed <= 0xffff_ffff ? parsed : null;
}

type Role = { kind: "lua"; appId: number } | { kind: "zip"; appId: number; order: number };

// Classifies a `Files/fix` file name. Only strict `{appid}.lua`, `{appid}.zip`, and
// `{appid}.zip.NNN` names qualify (so stray files like `readme.txt` are ignored).
export function classifyFixFile(name: string): Role | null {
  const lower = name.toLowerCase();
  if (lower.endsWith(".lua")) {
    const appId = parseU32(lower.slice(0, -4));
    return appId === null ? null : { kind: "lua", appId };
  }
  const partIndex = lower.lastIndexOf(".zip.");
  if (partIndex >= 0) {
    const order = parseU32(lower.slice(partIndex + ".zip.".length));
    const appId = parseU32(lower.slice(0, partIndex));
    return order === null || appId === null ? null : { kind: "zip", appId, order };
  }
  if (lower.endsWith(".zip")) {
    const appId = parseU32(lower.slice(0, -4));
    return appId === null ? null : { kind: "zip", appId, order: 0 };
  }
  return null;
}

export function resolveFixes(entries: ContentEntry[]): DenuvoFixEntry[] {
  const luas = new Map<number, FixFileRef>();
  const parts = new Map<number, Map<number, FixFileRef>>();

  for (const entry of entries) {
    if (entry.type !== "file") continue;
    const role = classifyFixFile(entry.name);
    if (!role) continue;
    const ref: FixFileRef = { name: entry.name, path: entry.path, sha: entry.sha, ...(entry.size !== undefined ? { size: entry.size } : {}) };
    if (role.kind === "lua") {
      luas.set(role.appId, ref);
    } else {
      let byOrder = parts.get(role.appId);
      if (!byOrder) parts.set(role.appId, (byOrder = new Map()));
      byOrder.set(role.order, ref);
    }
  }

  const fixes: DenuvoFixEntry[] = [];
  for (const [appid, lua] of luas) {
    const byOrder = parts.get(appid);
    if (!byOrder) continue; // a Lua without a zip is not a complete fix
    const zip_parts = [...byOrder.entries()].sort((a, b) => a[0] - b[0]).map(([, ref]) => ref);
    fixes.push({ appid, lua, zip_parts });
  }
  fixes.sort((a, b) => a.appid - b.appid);
  return fixes;
}

export class DenuvoFixesCache {
  private cached: CachedFixes | null = null;
  private inflight: Promise<CachedFixes> | null = null;

  constructor(
    private readonly config: Config,
    private readonly github: GitHubClient,
    private readonly log: FastifyBaseLogger,
  ) {}

  async getFixes(): Promise<DenuvoFixEntry[]> {
    return (await this.load()).fixes;
  }

  async resolveFile(name: string): Promise<FixFileRef | undefined> {
    return (await this.load()).byName.get(name.toLowerCase());
  }

  private async load(): Promise<CachedFixes> {
    const now = Date.now();
    if (this.cached && this.cached.expiresAt > now) return this.cached;
    if (this.inflight) return this.inflight;

    this.inflight = this.build()
      .then((built) => {
        this.cached = built;
        return built;
      })
      .catch((error) => {
        if (this.cached) {
          this.log.warn({ err: error }, "Denuvo-fixes manifest refresh failed; serving previous copy.");
          return this.cached;
        }
        throw error;
      })
      .finally(() => {
        this.inflight = null;
      });

    return this.inflight;
  }

  private async build(): Promise<CachedFixes> {
    const entries = await this.github.listDirectory(this.config.fixDirectory);
    const fixes = resolveFixes(entries);
    const byName = new Map<string, FixFileRef>();
    for (const fix of fixes) {
      byName.set(fix.lua.name.toLowerCase(), fix.lua);
      for (const part of fix.zip_parts) byName.set(part.name.toLowerCase(), part);
    }
    this.log.info({ count: fixes.length }, "Denuvo-fixes manifest built.");
    return { fixes, byName, expiresAt: Date.now() + this.config.fixesManifestTtlSeconds * 1000 };
  }
}
