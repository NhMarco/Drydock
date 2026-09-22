// Hubcap is the day-limited provider: what these cover is that its allowance is spent as rarely as
// possible — asked last, not at all while any copy is on disk, and what it does serve is kept until
// an ordinary provider can supply that app again.

import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, rm, readdir, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { createServer, type RequestListener, type Server } from "node:http";
import Fastify, { type FastifyBaseLogger } from "fastify";
import { FileCache } from "./fileCache.js";
import { HubcapClient } from "./hubcap.js";
import { ProviderCooldown } from "./merged.js";
import { UpstreamError } from "./upstream.js";
import { orderedSources } from "./config.js";
import type { Config } from "./config.js";
import { registerDepotRoutes, type DepotUpstreams } from "./routes/depot.js";
import type { RawStream } from "./depotbox.js";

const log = { info() {}, warn() {}, error() {} } as unknown as FastifyBaseLogger;
/** A ZIP-shaped buffer whose local file headers carry `names` — what the package check reads. */
function zipWith(names: string[], payload: string): Buffer {
  const parts: Buffer[] = [];
  for (const name of names) {
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(Buffer.byteLength(name), 26);
    parts.push(header, Buffer.from(name), Buffer.from(payload));
  }
  return Buffer.concat(parts);
}

// Real packages: the unlock plus a depot manifest, which is what the package check looks for.
const ZIP = zipWith(["70.lua", "71_1234.manifest"], "a depot package");
const HUBCAP_ZIP = zipWith(["70.lua", "71_5678.manifest"], "from hubcap");

function raw(body: Buffer, contentType = "application/zip"): RawStream {
  return { stream: Readable.from([body]), contentLength: body.length, contentType };
}

async function serve(listener: RequestListener): Promise<{ server: Server; base: string }> {
  const server = createServer(listener);
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  return { server, base: `http://127.0.0.1:${address.port}` };
}

async function stop(server: Server): Promise<void> {
  server.closeAllConnections();
  await new Promise<void>((resolve) => server.close(() => resolve()));
}

function hubcapConfig(base: string, overrides: Partial<Config> = {}): Config {
  return {
    hubcapBase: base,
    hubcapApiKey: "test-key",
    hubcapLibraryPageSize: 2,
    hubcapUsageTtlSeconds: 300,
    hubcapReserve: 0,
    gamelistTimeoutMs: 5_000,
    upstreamTimeoutMs: 5_000,
    depotPackageTimeoutMs: 5_000,
    ...overrides,
  } as unknown as Config;
}

test("the library is walked to the end and only usable games survive", async () => {
  const pages = [
    {
      total_count: 5,
      games: [
        { game_id: "10", game_name: "Half-Life", manifest_available: true },
        { game_id: "20", game_name: "Team Fortress", manifest_available: true },
      ],
    },
    {
      total_count: 5,
      games: [
        { game_id: "not-a-number", game_name: "Broken", manifest_available: true },
        { game_id: "30", game_name: "  ", manifest_available: true },
      ],
    },
    {
      total_count: 5,
      games: [
        { game_id: "40", game_name: "No manifest", manifest_available: false },
        { game_id: "10", game_name: "Half-Life (again)", manifest_available: true },
      ],
    },
  ];
  const offsets: number[] = [];
  const { server, base } = await serve((req, res) => {
    const url = new URL(req.url ?? "", "http://localhost");
    assert.equal(req.headers.authorization, "Bearer test-key");
    const offset = Number(url.searchParams.get("offset"));
    offsets.push(offset);
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(pages[offset / 2] ?? { total_count: 5, games: [] }));
  });
  try {
    const client = new HubcapClient(hubcapConfig(base));
    const parsed = JSON.parse(await client.fetchGamelist()) as { games: { appid: number; name: string }[] };
    assert.deepEqual(
      parsed.games,
      [
        { appid: 10, name: "Half-Life", tags: [] },
        { appid: 20, name: "Team Fortress", tags: [] },
      ],
      "a broken id, a blank name, an unavailable manifest and a repeat are all dropped",
    );
    assert.deepEqual(offsets, [0, 2, 4], "every page is walked until the count is reached");
  } finally {
    await stop(server);
  }
});

test("the daily allowance is read once and gates the provider", async () => {
  let reads = 0;
  const quota = { single: { usage: 24, limit: 25, remaining: 1 } };
  const { server, base } = await serve((_req, res) => {
    reads += 1;
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(quota));
  });
  try {
    const plenty = new HubcapClient(hubcapConfig(base));
    assert.equal(await plenty.hasAllowance(), true);
    assert.equal(await plenty.hasAllowance(), true);
    assert.equal(reads, 1, "the reading is reused inside its TTL");

    const reserved = new HubcapClient(hubcapConfig(base, { hubcapReserve: 1 }));
    assert.equal(await reserved.hasAllowance(), false, "the reserve is left untouched");
  } finally {
    await stop(server);
  }
});

test("an unreadable allowance does not block the provider", async () => {
  const { server, base } = await serve((_req, res) => {
    res.writeHead(500);
    res.end("nope");
  });
  try {
    const client = new HubcapClient(hubcapConfig(base));
    assert.equal(await client.hasAllowance(), true);
  } finally {
    await stop(server);
  }
});

test("a day-limited provider is moved behind the others whatever order it is given", () => {
  assert.deepEqual(orderedSources(["hubcap", "ryu", "depotbox"]), ["ryu", "depotbox", "hubcap"]);
  assert.deepEqual(orderedSources(["ryu", "steamtools"]), ["ryu", "steamtools"]);
});

/** A depot route wired to fakes, so each test can decide what every provider does. */
async function depotApp(
  dir: string,
  behaviour: {
    ryu: () => Promise<RawStream>;
    hubcap: () => Promise<RawStream>;
    allowance?: () => Promise<boolean>;
    ttlMs?: number;
  },
): Promise<{ app: ReturnType<typeof Fastify>; calls: { ryu: number; hubcap: number } }> {
  const calls = { ryu: 0, hubcap: 0 };
  const app = Fastify();
  const upstreams = {
    ryu: {
      downloadDepotPackage: async () => {
        calls.ryu += 1;
        return behaviour.ryu();
      },
    },
    depotbox: { downloadDepotPackage: async () => raw(ZIP) },
    steamtools: { downloadManifestZip: async () => raw(ZIP) },
    hubcap: {
      downloadDepotPackage: async () => {
        calls.hubcap += 1;
        return behaviour.hubcap();
      },
      hasAllowance: behaviour.allowance ?? (async () => true),
    },
  } as unknown as DepotUpstreams;
  const cache = new FileCache(dir, behaviour.ttlMs ?? 60_000, log);
  // Deliberately listed first: it still has to end up last.
  registerDepotRoutes(app, upstreams, ["hubcap", "ryu"], cache, async () => {}, new ProviderCooldown());
  return { app, calls };
}

test("hubcap is only asked once the ordinary providers have failed", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-hubcap-"));
  const { app, calls } = await depotApp(dir, {
    ryu: async () => raw(ZIP),
    hubcap: async () => raw(HUBCAP_ZIP),
  });
  try {
    const response = await app.inject({ url: "/v1/depot/package/70" });
    assert.equal(response.statusCode, 200);
    assert.equal(response.headers["x-depot-source"], "ryu");
    assert.equal(response.headers["x-cache"], "MISS");
    assert.deepEqual(response.rawPayload, ZIP);
    assert.equal(calls.hubcap, 0, "its allowance is not spent on an app another provider has");
  } finally {
    await app.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("what hubcap serves is kept until an ordinary provider can supply it again", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-hubcap-"));
  let ryuWorks = false;
  // A TTL of zero means every later request counts as expired — the kept copy is all that stands
  // between the app and another day-limited request.
  const { app, calls } = await depotApp(dir, {
    ryu: async () => {
      if (!ryuWorks) throw new UpstreamError(503, "ryu is down");
      return raw(ZIP);
    },
    hubcap: async () => raw(HUBCAP_ZIP),
    ttlMs: 1,
  });
  try {
    const first = await app.inject({ url: "/v1/depot/package/70" });
    assert.equal(first.headers["x-depot-source"], "hubcap");
    assert.equal(first.headers["x-cache"], "KEPT");
    assert.deepEqual(first.rawPayload, HUBCAP_ZIP);

    // Expired by now, and Ryu is still down: the kept copy answers, and no allowance is spent.
    await new Promise((resolve) => setTimeout(resolve, 5));
    const again = await app.inject({ url: "/v1/depot/package/70" });
    assert.equal(again.statusCode, 200);
    assert.equal(again.headers["x-cache"], "KEPT");
    assert.deepEqual(again.rawPayload, HUBCAP_ZIP);
    assert.equal(calls.hubcap, 1, "the kept copy is served instead of asking again");

    // Ryu recovers: its package replaces the kept one, and the entry goes back to normal caching.
    ryuWorks = true;
    const recovered = await app.inject({ url: "/v1/depot/package/70" });
    assert.equal(recovered.headers["x-depot-source"], "ryu");
    assert.equal(recovered.headers["x-cache"], "MISS");
    assert.deepEqual(recovered.rawPayload, ZIP);
    const sidecars = (await readdir(join(dir, "filecache"))).filter((name) => name.endsWith(".json"));
    const meta = JSON.parse(await readFile(join(dir, "filecache", sidecars[0]), "utf8")) as { kept?: boolean };
    assert.notEqual(meta.kept, true, "the replacement expires like any other package");
  } finally {
    await app.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("with the allowance gone, hubcap is skipped rather than asked", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-hubcap-"));
  const { app, calls } = await depotApp(dir, {
    ryu: async () => {
      throw new UpstreamError(503, "ryu is down");
    },
    hubcap: async () => raw(HUBCAP_ZIP),
    allowance: async () => false,
  });
  try {
    const response = await app.inject({ url: "/v1/depot/package/70" });
    assert.equal(response.statusCode, 502);
    assert.equal(calls.hubcap, 0);
  } finally {
    await app.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("a kept entry survives the sweep that clears expired ones", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-hubcap-"));
  try {
    const cache = new FileCache(dir, 1, log);
    await cache.put("kept", Readable.from([HUBCAP_ZIP]), "application/zip", true);
    await cache.put("ordinary", Readable.from([ZIP]), "application/zip");
    await new Promise((resolve) => setTimeout(resolve, 10));
    const swept = await cache.sweep();
    assert.equal(swept.removed, 1, "only the ordinary entry is evicted");
    assert.equal(await cache.get("kept"), null, "it is still expired for the normal path");
    const stale = await cache.getStale("kept");
    assert.ok(stale && stale.kept, "but it is there for the caller that has nothing else");
    assert.equal(await cache.getStale("ordinary"), null);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
