// Central, validated configuration for the Drydock proxy. Everything is read from the
// environment so no secret is ever committed. Missing required values fail fast at boot.

import { resolve } from "node:path";

function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`Missing required environment variable: ${name}`);
  return value;
}

function optional(name: string, fallback: string): string {
  const value = process.env[name]?.trim();
  return value && value.length > 0 ? value : fallback;
}

function integer(name: string, fallback: number): number {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) throw new Error(`Invalid integer for ${name}: ${raw}`);
  return parsed;
}

function boolean(name: string, fallback: boolean): boolean {
  const raw = process.env[name]?.trim().toLowerCase();
  if (raw === undefined || raw === "") return fallback;
  return raw === "1" || raw === "true" || raw === "yes";
}

// The upstreams the depot-package route can draw from. Order in `DEPOT_PACKAGE_SOURCES` is the
// try order; listing a name enables it, omitting it disables it.
export const DEPOT_PACKAGE_SOURCE_NAMES = ["ryu", "depotbox", "steamtools"] as const;
export type DepotPackageSourceName = (typeof DEPOT_PACKAGE_SOURCE_NAMES)[number];

// Parses the ordered, comma-separated source list, keeping only known names and dropping duplicates
// so a stray/typo'd entry can never break startup. Falls back to `fallback` when nothing valid is set.
function depotSourceList(name: string, fallback: DepotPackageSourceName[]): DepotPackageSourceName[] {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const seen = new Set<DepotPackageSourceName>();
  for (const part of raw.split(",").map((value) => value.trim().toLowerCase())) {
    if ((DEPOT_PACKAGE_SOURCE_NAMES as readonly string[]).includes(part)) {
      seen.add(part as DepotPackageSourceName);
    }
  }
  return seen.size > 0 ? [...seen] : fallback;
}

// One or more HMAC secrets (comma-separated) so a new secret can be rolled out
// while old app builds still validate against the previous one.
function secretList(name: string): string[] {
  const secrets = required(name)
    .split(",")
    .map((value) => value.trim())
    .filter((value) => value.length > 0);
  if (secrets.length === 0) throw new Error(`No usable secrets found in ${name}`);
  return secrets;
}

export interface Config {
  host: string;
  port: number;
  trustProxy: boolean;
  requireAuth: boolean;

  upstreamBase: string;
  upstreamApiKey: string;
  // Steam Web API key for the achievement schema (emulator-template generator). Optional: when
  // unset, `/v1/app-schema/:appid` returns an empty achievement list.
  steamWebApiKey: string;
  upstreamTimeoutMs: number;
  gamelistTimeoutMs: number;
  // The depot package (manifests + key) is generated on demand upstream and can take minutes.
  depotPackageTimeoutMs: number;
  // How long user-facing upstream files (depot packages, magicfiles, emu binaries) are cached on
  // disk before being refetched, to reduce load on the upstream providers.
  fileCacheTtlSeconds: number;

  // DepotBox is the upstream for the gamelist, per-app Lua, game fixes, and the depot package
  // (manifests + key) that Drydock' native depot download uses.
  depotboxBase: string;
  depotboxApiKey: string;

  // Ryu (generator.ryuu.lol) is an additional depot-package upstream. Its `secure_download` returns
  // the same manifests+lua ZIP shape. The reseller auth code is passed as a query parameter.
  ryuBase: string;
  ryuAuthCode: string;

  // Global, ordered list of ACTIVE providers (`PROVIDER_SOURCES`). One toggle governs everything a
  // provider can supply — the gamelist, per-app Lua, and depot-package downloads. Any of "ryu",
  // "depotbox", "steamtools"; unknown names are dropped. A provider not listed here is fully off (it
  // never feeds the gamelist/Lua nor is tried for depot downloads). Default `ryu` only; add more to
  // re-enable (e.g. "ryu,depotbox,steamtools"). Falls back to the legacy `DEPOT_PACKAGE_SOURCES`.
  providerSources: DepotPackageSourceName[];

  githubOwner: string;
  githubRepo: string;
  githubBranch: string;
  githubToken: string;
  // Repo directory of the per-app fixes (`{appid}.lua` + `{appid}.zip[.NNN]`).
  fixDirectory: string;
  fixesManifestTtlSeconds: number;
  // Repo directory of the per-app Ubisoft "magicfiles" (`{appid}.zip`), relayed to the client.
  magicfilesDirectory: string;
  // Repo file listing per-app repacks (`{ "<appid>": [{ repacker, link }] }`) the proxy relays.
  repacksPath: string;
  repacksTtlSeconds: number;

  hmacSecrets: string[];
  hmacWindowSeconds: number;

  gamelistRefreshMinutes: number;
  filterNsfw: boolean;
  luaCacheTtlSeconds: number;
  appInfoCacheTtlSeconds: number;

  // Flat repo directory of the Steam Service (OST) payload the proxy relays to clients.
  serviceDirectory: string;
  serviceVersionPath: string;
  serviceManifestTtlSeconds: number;

  luaRateWindowSeconds: number;
  luaRateMax: number;
  globalRateWindowSeconds: number;
  globalRateMax: number;

  dataDir: string;
}

export function loadConfig(): Config {
  const requireAuth = boolean("REQUIRE_AUTH", true);
  return {
    host: optional("HOST", "0.0.0.0"),
    port: integer("PORT", 8080),
    trustProxy: boolean("TRUST_PROXY", true),
    requireAuth,

    upstreamBase: optional("STEAMTOOLS_API_BASE", "https://api.steamtools.app").replace(/\/+$/, ""),
    upstreamApiKey: required("STEAMTOOLS_API_KEY"),
    steamWebApiKey: optional("STEAM_WEB_API_KEY", ""),
    upstreamTimeoutMs: integer("UPSTREAM_TIMEOUT_MS", 30_000),
    gamelistTimeoutMs: integer("GAMELIST_TIMEOUT_MS", 120_000),
    depotPackageTimeoutMs: integer("DEPOT_PACKAGE_TIMEOUT_MS", 300_000),
    fileCacheTtlSeconds: integer("FILE_CACHE_TTL_SECONDS", 86_400),

    depotboxBase: optional("DEPOTBOX_BASE", "https://depotbox.org").replace(/\/+$/, ""),
    depotboxApiKey: required("DEPOTBOX_API_KEY"),

    ryuBase: optional("RYU_API_BASE", "https://generator.ryuu.lol").replace(/\/+$/, ""),
    ryuAuthCode: optional("RYU_AUTH_CODE", ""),

    // One global provider toggle for gamelist + Lua + depot. Default Ryu only; DepotBox/SteamTools
    // stay wired and come back by adding them here. `DEPOT_PACKAGE_SOURCES` is honoured as a fallback.
    providerSources: depotSourceList("PROVIDER_SOURCES", depotSourceList("DEPOT_PACKAGE_SOURCES", ["ryu"])),

    githubOwner: optional("GITHUB_OWNER", "NhMarco"),
    githubRepo: optional("GITHUB_REPO", "MFB"),
    githubBranch: optional("GITHUB_BRANCH", "main"),
    githubToken: required("GITHUB_TOKEN"),
    fixDirectory: optional("FIX_DIRECTORY", "Files/fix").replace(/^\/+|\/+$/g, ""),
    fixesManifestTtlSeconds: integer("FIXES_MANIFEST_TTL_SECONDS", 300),
    magicfilesDirectory: optional("MAGICFILES_DIRECTORY", "Files/magicfiles").replace(/^\/+|\/+$/g, ""),
    repacksPath: optional("REPACKS_PATH", "Files/repacks.json").replace(/^\/+/, ""),
    repacksTtlSeconds: integer("REPACKS_TTL_SECONDS", 300),

    // Auth is only truly optional when explicitly disabled (local testing).
    hmacSecrets: requireAuth ? secretList("DRYDOCK_HMAC_SECRET") : [],
    hmacWindowSeconds: integer("HMAC_WINDOW_SECONDS", 60),

    gamelistRefreshMinutes: integer("GAMELIST_REFRESH_MINUTES", 60),
    filterNsfw: boolean("FILTER_NSFW", false),
    luaCacheTtlSeconds: integer("LUA_CACHE_TTL_SECONDS", 900),
    appInfoCacheTtlSeconds: integer("APP_INFO_CACHE_TTL_SECONDS", 86_400),

    serviceDirectory: optional("SERVICE_DIRECTORY", "Files/OST").replace(/^\/+|\/+$/g, ""),
    serviceVersionPath: optional("SERVICE_VERSION_PATH", "Files/OST/version.json").replace(/^\/+/, ""),
    serviceManifestTtlSeconds: integer("SERVICE_MANIFEST_TTL_SECONDS", 300),

    luaRateWindowSeconds: integer("LUA_RATE_WINDOW_SECONDS", 60),
    luaRateMax: integer("LUA_RATE_MAX", 1),
    globalRateWindowSeconds: integer("GLOBAL_RATE_WINDOW_SECONDS", 60),
    globalRateMax: integer("GLOBAL_RATE_MAX", 120),

    dataDir: resolve(optional("DATA_DIR", "./data")),
  };
}
