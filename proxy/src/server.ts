// Drydock proxy entry point. Wires configuration, security middleware, the gamelist cache,
// and the routes, then starts listening. See README.md for the full request contract.

import Fastify from "fastify";
import helmet from "@fastify/helmet";
import rateLimit from "@fastify/rate-limit";
import { isLastResort, loadConfig, orderedSources, type OrdinarySourceName } from "./config.js";
import { SteamToolsClient } from "./upstream.js";
import { DepotBoxClient } from "./depotbox.js";
import { RyuClient } from "./ryu.js";
import { HubcapClient } from "./hubcap.js";
import { GitHubClient } from "./github.js";
import { MergedGamelistSource, MergedLuaSource, ProviderCooldown } from "./merged.js";
import type { LuaSource } from "./depotbox.js";
import { GamelistCache } from "./gamelistCache.js";
import { ServiceCache } from "./serviceCache.js";
import { EmuCache } from "./emuCache.js";
import { DenuvoFixesCache } from "./denuvoFixesCache.js";
import { RepacksCache } from "./repacksCache.js";
import { createAuthHook } from "./auth.js";
import { FileCache } from "./fileCache.js";
import { registerHealthRoute } from "./routes/health.js";
import { registerGamelistRoute } from "./routes/gamelist.js";
import { registerLuaRoute } from "./routes/lua.js";
import { registerServiceRoutes } from "./routes/service.js";
import { registerEmuRoutes } from "./routes/emu.js";
import { registerMagicfilesRoute } from "./routes/magicfiles.js";
import { registerDenuvoFixesRoutes } from "./routes/denuvoFixes.js";
import { registerRepacksRoute } from "./routes/repacks.js";
import { registerAppInfoRoute } from "./routes/appinfo.js";
import { registerSchemaRoute } from "./routes/schema.js";
import { registerDepotRoutes } from "./routes/depot.js";

async function main(): Promise<void> {
  const config = loadConfig();
  const isProduction = process.env.NODE_ENV === "production";

  const app = Fastify({
    trustProxy: config.trustProxy,
    // A gzipped gamelist can be a few MB; keep the body limit tiny since we only accept GETs.
    bodyLimit: 4096,
    logger: {
      level: process.env.LOG_LEVEL ?? "info",
      ...(isProduction ? {} : { transport: { target: "pino-pretty" } }),
    },
  });

  await app.register(helmet, { contentSecurityPolicy: false });

  // Global per-IP limiter. Individual routes tighten or disable this via their own config.
  await app.register(rateLimit, {
    global: true,
    max: config.globalRateMax,
    timeWindow: config.globalRateWindowSeconds * 1000,
    keyGenerator: (req) => req.ip,
  });

  const client = new SteamToolsClient(config);
  const depotbox = new DepotBoxClient(config);
  const ryu = new RyuClient(config);
  const hubcap = new HubcapClient(config);
  const github = new GitHubClient(config);

  // One global provider toggle (PROVIDER_SOURCES, ordered) governs the gamelist, Lua and depot. Each
  // active provider contributes what it can; a deactivated one (not in the list) never feeds any of
  // them. Order = priority (index 0 wins on shared AppIDs / is tried first for Lua), except that a
  // day-limited provider is always moved to the back and contributes no Lua at all.
  const providerRegistry = { ryu, depotbox, steamtools: client, hubcap };
  // Day-limited providers are moved to the back: last to be asked for a package, and lowest priority
  // in the gamelist, where they only add the apps nobody else lists.
  const activeProviders = orderedSources(config.providerSources).map((name) => ({
    name,
    source: providerRegistry[name],
  }));
  app.log.info({ providers: activeProviders.map((p) => p.name) }, "Active providers (gamelist / lua / depot).");
  const gamelistSource = new MergedGamelistSource(activeProviders, app.log);
  // Shared, so a provider that rate limits Lua requests is also left alone for depot packages.
  const cooldown = new ProviderCooldown();
  // Hubcap has no Lua endpoint of its own — its unlock only comes inside the manifest ZIP, and
  // fetching that to answer a Lua request would spend a day's allowance on it.
  const luaRegistry: Record<OrdinarySourceName, LuaSource> = { ryu, depotbox, steamtools: client };
  const luaProviders = orderedSources(config.providerSources)
    .filter((name): name is OrdinarySourceName => !isLastResort(name))
    .map((name) => ({ name, source: luaRegistry[name] }));
  const luaSource = new MergedLuaSource(luaProviders, app.log, cooldown);
  const cache = new GamelistCache(config, gamelistSource, app.log);
  const serviceCache = new ServiceCache(config, github, app.log);
  const emuCache = new EmuCache(config, github, app.log);
  const denuvoFixesCache = new DenuvoFixesCache(config, github, app.log);
  const repacksCache = new RepacksCache(config, github, app.log);
  // Disk cache (24h) for user-facing upstream files, so each is pulled from its provider at most
  // once a day.
  const fileCache = new FileCache(config.dataDir, config.fileCacheTtlSeconds * 1000, app.log);
  const authHook = createAuthHook(config);

  registerHealthRoute(app, cache);
  registerGamelistRoute(app, config, cache, authHook);
  registerLuaRoute(app, config, luaSource, authHook);
  registerServiceRoutes(app, github, serviceCache, authHook);
  registerEmuRoutes(app, github, emuCache, authHook);
  registerMagicfilesRoute(app, config, github, fileCache, authHook);
  registerDenuvoFixesRoutes(app, github, denuvoFixesCache, authHook);
  registerRepacksRoute(app, repacksCache, authHook);
  // App info comes from SteamTools alone and spends its quota, so it exists only while SteamTools is on.
  if (config.providerSources.includes("steamtools")) registerAppInfoRoute(app, config, client, authHook);
  registerSchemaRoute(app, config, fileCache, authHook);
  registerDepotRoutes(app, providerRegistry, config.providerSources, fileCache, authHook, cooldown);

  if (!config.requireAuth) {
    app.log.warn("REQUIRE_AUTH is disabled — HMAC verification is OFF. Use this only for local testing.");
  }
  if (!config.githubToken) {
    // Not fatal: the gamelist, Lua and depot routes work without it. Say so explicitly, because the
    // alternative is a self-hoster discovering it as four endpoints mysteriously returning 404.
    app.log.warn(
      { owner: config.githubOwner, repo: config.githubRepo },
      "GITHUB_TOKEN is unset — /v1/service/*, /v1/emu/*, /v1/denuvo-fixes*, /v1/repacks and " +
        "/v1/magicfiles will fail if the payload repository is private. Everything else works.",
    );
  }

  // Bring the gamelist online before we accept traffic so early clients don't all 503.
  await cache.start();
  // Begin evicting expired file-cache entries. Without this the depot ZIPs and magicfiles only ever
  // accumulated — an expired entry counted as a miss but its bytes stayed on disk forever.
  fileCache.start();

  const shutdown = async (signal: string): Promise<void> => {
    app.log.info({ signal }, "Shutting down.");
    cache.stop();
    fileCache.stop();
    await app.close();
    process.exit(0);
  };
  process.on("SIGTERM", () => void shutdown("SIGTERM"));
  process.on("SIGINT", () => void shutdown("SIGINT"));

  await app.listen({ host: config.host, port: config.port });
}

main().catch((error) => {
  // eslint-disable-next-line no-console
  console.error("Fatal startup error:", error);
  process.exit(1);
});
