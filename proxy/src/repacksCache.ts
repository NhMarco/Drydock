// Builds and briefly caches the per-app "repacks" list from a single JSON file in the MFB repo
// (`Files/repacks.json` by default). Each app maps to one or more external download sources:
//
//   { "<appid>": [ { "repacker": "FitGirl", "link": "https://..." }, ... ], ... }
//
// The proxy validates and normalizes this hand-maintained map into a stable array response for
// the client (mirroring the shape of `/v1/fixes`). Only http(s) links survive validation, since
// the client opens them in the user's browser — anything else (file:, javascript:, steam:, …) is
// dropped so a malformed or hostile entry can never trigger a dangerous URI.

import type { FastifyBaseLogger } from "fastify";
import type { Config } from "./config.js";
import type { GitHubClient } from "./github.js";
import { GitHubError } from "./github.js";

export interface RepackSource {
  repacker: string;
  link: string;
}

export interface RepackApp {
  appid: number;
  sources: RepackSource[];
}

// Bounds so a broken or hostile file cannot balloon memory or the response.
const MAX_APPS = 5000;
const MAX_SOURCES_PER_APP = 25;
const MAX_FIELD_LENGTH = 2048;

// Removes trailing commas (`,}` / `,]`) so the hand-maintained file survives the single most
// common JSON mistake. A tiny scanner tracks string state, so a comma inside a value (e.g. a URL
// or a repacker name that contains `,]`) is never touched — only structural trailing commas go.
export function stripTrailingCommas(text: string): string {
  let out = "";
  let inString = false;
  let escaped = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (escaped) escaped = false;
      else if (ch === "\\") escaped = true;
      else if (ch === '"') inString = false;
      continue;
    }
    if (ch === '"') {
      inString = true;
      out += ch;
      continue;
    }
    if (ch === ",") {
      let j = i + 1;
      while (j < text.length && /\s/.test(text[j]!)) j += 1;
      if (j < text.length && (text[j] === "}" || text[j] === "]")) continue; // drop trailing comma
    }
    out += ch;
  }
  return out;
}

function parseAppId(value: string): number | null {
  if (!/^[0-9]{1,10}$/.test(value)) return null;
  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed > 0 && parsed <= 0xffff_ffff ? parsed : null;
}

function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:";
  } catch {
    return false;
  }
}

// Validates and normalizes the raw `{ "<appid>": [{repacker, link}] }` map into a sorted array.
// Invalid apps/sources are skipped (and counted) rather than failing the whole list, so one bad
// hand-edited row never takes the feature offline.
export function normalizeRepacks(raw: unknown, log?: FastifyBaseLogger): RepackApp[] {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new GitHubError(502, "The repacks file must be a JSON object keyed by App ID.");
  }
  const apps: RepackApp[] = [];
  let skipped = 0;
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (apps.length >= MAX_APPS) break;
    const appid = parseAppId(key);
    if (appid === null || !Array.isArray(value)) {
      skipped += 1;
      continue;
    }
    const sources: RepackSource[] = [];
    const seenLinks = new Set<string>();
    for (const entry of value) {
      if (sources.length >= MAX_SOURCES_PER_APP) break;
      if (typeof entry !== "object" || entry === null) {
        skipped += 1;
        continue;
      }
      const repacker = typeof (entry as Record<string, unknown>).repacker === "string"
        ? ((entry as Record<string, unknown>).repacker as string).trim()
        : "";
      const link = typeof (entry as Record<string, unknown>).link === "string"
        ? ((entry as Record<string, unknown>).link as string).trim()
        : "";
      if (
        !repacker ||
        !link ||
        repacker.length > MAX_FIELD_LENGTH ||
        link.length > MAX_FIELD_LENGTH ||
        !isHttpUrl(link) ||
        seenLinks.has(link.toLowerCase())
      ) {
        skipped += 1;
        continue;
      }
      seenLinks.add(link.toLowerCase());
      sources.push({ repacker, link });
    }
    if (sources.length > 0) apps.push({ appid, sources });
  }
  apps.sort((a, b) => a.appid - b.appid);
  if (skipped > 0) log?.warn({ skipped }, "Skipped invalid repack entries.");
  return apps;
}

interface CachedRepacks {
  repacks: RepackApp[];
  expiresAt: number;
}

export class RepacksCache {
  private cached: CachedRepacks | null = null;
  private inflight: Promise<CachedRepacks> | null = null;

  constructor(
    private readonly config: Config,
    private readonly github: GitHubClient,
    private readonly log: FastifyBaseLogger,
  ) {}

  async getRepacks(): Promise<RepackApp[]> {
    return (await this.load()).repacks;
  }

  private async load(): Promise<CachedRepacks> {
    const now = Date.now();
    if (this.cached && this.cached.expiresAt > now) return this.cached;
    if (this.inflight) return this.inflight;

    this.inflight = this.build()
      .then((built) => {
        this.cached = built;
        return built;
      })
      .catch((error) => {
        // On refresh failure keep serving the last good list if we still have one.
        if (this.cached) {
          this.log.warn({ err: error }, "Repacks refresh failed; serving previous copy.");
          return this.cached;
        }
        throw error;
      })
      .finally(() => {
        this.inflight = null;
      });

    return this.inflight;
  }

  private async build(): Promise<CachedRepacks> {
    // A missing file simply means "no repacks" rather than an error, matching the fixes flow.
    let text: string;
    try {
      text = await this.github.fetchRawText(this.config.repacksPath);
    } catch (error) {
      if (error instanceof GitHubError && error.status === 404) {
        this.log.info("No repacks file present; serving an empty list.");
        return { repacks: [], expiresAt: Date.now() + this.config.repacksTtlSeconds * 1000 };
      }
      throw error;
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(stripTrailingCommas(text));
    } catch {
      throw new GitHubError(502, "The repacks file is not valid JSON.");
    }
    const repacks = normalizeRepacks(parsed, this.log);
    this.log.info({ count: repacks.length }, "Repacks list built.");
    return { repacks, expiresAt: Date.now() + this.config.repacksTtlSeconds * 1000 };
  }
}
