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
  if (["1", "true", "yes"].includes(raw)) return true;
  if (["0", "false", "no"].includes(raw)) return false;
  throw new Error(`Invalid boolean for ${name}: ${raw}`);
}

// The upstreams the depot-package route can draw from. Order in `DEPOT_PACKAGE_SOURCES` is the
// try order; listing a name enables it, omitting it disables it.
export const DEPOT_PACKAGE_SOURCE_NAMES = ["ryu", "depotbox", "steamtools", "hubcap"] as const;
export type DepotPackageSourceName = (typeof DEPOT_PACKAGE_SOURCE_NAMES)[number];

// Providers whose allowance is measured per *day*, not per minute. They are always tried after every
// other source, wherever they are listed in `PROVIDER_SOURCES`, and they only feed the gamelist with
// the apps nobody else has: a request spent on an app another provider could have served is an app
// the proxy cannot serve at all for the rest of the day.
export const LAST_RESORT_SOURCE_NAMES = ["hubcap"] as const satisfies readonly DepotPackageSourceName[];
export type LastResortSourceName = (typeof LAST_RESORT_SOURCE_NAMES)[number];
/** Every provider that is not day-limited — the ones that also answer `/v1/lua`. */
export type OrdinarySourceName = Exclude<DepotPackageSourceName, LastResortSourceName>;

export function isLastResort(name: DepotPackageSourceName): name is LastResortSourceName {
  return (LAST_RESORT_SOURCE_NAMES as readonly string[]).includes(name);
}

/** The configured providers with the day-limited ones moved to the back, order otherwise kept. */
export function orderedSources(sources: DepotPackageSourceName[]): DepotPackageSourceName[] {
  return [...sources.filter((name) => !isLastResort(name)), ...sources.filter(isLastResort)];
}

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
  // DepotBox generates a Lua on demand, which can take minutes. The Drydock client waits 180 s for a
  // whole Lua request, so this plus the other providers' timeouts has to stay below that.
  depotboxLuaTimeoutMs: number;
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

  // Hubcap Manifest (hubcapmanifest.com): the last-resort package source and an extra gamelist. Its
  // manifest downloads are capped per day, so what it serves is kept until another provider can.
  hubcapBase: string;
  hubcapApiKey: string;
  // Entries per `/api/v1/library` page. The API accepts large pages (20k answered in ~2 s), and
  // fewer, bigger pages beat many small ones for a library of ~157k apps.
  hubcapLibraryPageSize: number;
  // How long a reading of the daily allowance is reused before asking again.
  hubcapUsageTtlSeconds: number;
  // Requests of the daily allowance to leave untouched, so a burst cannot spend the last of it.
  hubcapReserve: number;

  // Global, ordered list of ACTIVE providers (`PROVIDER_SOURCES`). One toggle governs everything a
  // provider can supply — the gamelist, per-app Lua, and depot-package downloads. Any of "ryu",
  // "depotbox", "steamtools", "hubcap"; unknown names are dropped. A provider not listed here is
  // fully off (it never feeds the gamelist/Lua nor is tried for depot downloads). Default `ryu`
  // only; add more to re-enable (e.g. "ryu,depotbox,steamtools,hubcap"). A day-limited provider is
  // moved to the back of the order whatever position it is given (see `orderedSources`). Falls back
  // to the legacy `DEPOT_PACKAGE_SOURCES`.
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
  // Flat repo directory of the emulator DLLs the cracker deploys into a game folder.
  emuDirectory: string;
  emuListingTtlSeconds: number;
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

  // Resolve the active providers first: a provider's credential is only mandatory when that
  // provider is actually switched on. Previously every provider key was unconditionally required,
  // so running the default Ryu-only setup still demanded a SteamTools key and a DepotBox key that
  // were never used — while `RYU_AUTH_CODE`, the one credential the default configuration does
  // need, was optional and could be omitted silently.
  const providerSources = depotSourceList("PROVIDER_SOURCES", depotSourceList("DEPOT_PACKAGE_SOURCES", ["ryu"]));
  const activeProvider = (name: DepotPackageSourceName): boolean => providerSources.includes(name);
  const credential = (name: string, provider: DepotPackageSourceName): string => {
    if (!activeProvider(provider)) return optional(name, "");
    const value = required(name);
    // A value copied unchanged from .env.example would otherwise only fail on the first request.
    if (/^(your_|replace_with|stpriv_your|github_pat_your)/i.test(value)) {
      throw new Error(`${name} still holds the placeholder from .env.example`);
    }
    return value;
  };

  return {
    host: optional("HOST", "0.0.0.0"),
    port: integer("PORT", 8080),
    trustProxy: boolean("TRUST_PROXY", true),
    requireAuth,

    upstreamBase: optional("STEAMTOOLS_API_BASE", "https://api.steamtools.app").replace(/\/+$/, ""),
    upstreamApiKey: credential("STEAMTOOLS_API_KEY", "steamtools"),
    steamWebApiKey: optional("STEAM_WEB_API_KEY", ""),
    upstreamTimeoutMs: integer("UPSTREAM_TIMEOUT_MS", 30_000),
    gamelistTimeoutMs: integer("GAMELIST_TIMEOUT_MS", 120_000),
    depotPackageTimeoutMs: integer("DEPOT_PACKAGE_TIMEOUT_MS", 300_000),
    depotboxLuaTimeoutMs: integer("DEPOTBOX_LUA_TIMEOUT_MS", 90_000),
    fileCacheTtlSeconds: integer("FILE_CACHE_TTL_SECONDS", 86_400),

    depotboxBase: optional("DEPOTBOX_BASE", "https://depotbox.org").replace(/\/+$/, ""),
    depotboxApiKey: credential("DEPOTBOX_API_KEY", "depotbox"),

    ryuBase: optional("RYU_API_BASE", "https://generator.ryuu.lol").replace(/\/+$/, ""),
    ryuAuthCode: credential("RYU_AUTH_CODE", "ryu"),

    hubcapBase: optional("HUBCAP_API_BASE", "https://hubcapmanifest.com").replace(/\/+$/, ""),
    hubcapApiKey: credential("HUBCAP_API_KEY", "hubcap"),
    hubcapLibraryPageSize: Math.min(Math.max(integer("HUBCAP_LIBRARY_PAGE_SIZE", 20_000), 1), 50_000),
    hubcapUsageTtlSeconds: integer("HUBCAP_USAGE_TTL_SECONDS", 300),
    hubcapReserve: integer("HUBCAP_RESERVE", 0),

    // One global provider toggle for gamelist + Lua + depot. Default Ryu only; DepotBox/SteamTools
    // stay wired and come back by adding them here. `DEPOT_PACKAGE_SOURCES` is honoured as a fallback.
    providerSources,

    githubOwner: optional("GITHUB_OWNER", "NhMarco"),
    githubRepo: optional("GITHUB_REPO", "MFB"),
    githubBranch: optional("GITHUB_BRANCH", "main"),
    githubToken: optional("GITHUB_TOKEN", ""),
    fixDirectory: optional("FIX_DIRECTORY", "Files/fix").replace(/^\/+|\/+$/g, ""),
    fixesManifestTtlSeconds: integer("FIXES_MANIFEST_TTL_SECONDS", 300),
    magicfilesDirectory: optional("MAGICFILES_DIRECTORY", "Files/magicfiles").replace(/^\/+|\/+$/g, ""),
    emuDirectory: optional("EMU_DIRECTORY", "Files/dlls").replace(/^\/+|\/+$/g, ""),
    emuListingTtlSeconds: integer("EMU_LISTING_TTL_SECONDS", 300),
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
    luaRateMax: integer("LUA_RATE_MAX", 30),
    globalRateWindowSeconds: integer("GLOBAL_RATE_WINDOW_SECONDS", 60),
    globalRateMax: integer("GLOBAL_RATE_MAX", 120),

    dataDir: resolve(optional("DATA_DIR", "./data")),
  };
}
