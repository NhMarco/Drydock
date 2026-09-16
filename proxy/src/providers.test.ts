import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable, Writable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createServer, type IncomingHttpHeaders, type RequestListener, type Server } from "node:http";
import Fastify, { type FastifyBaseLogger } from "fastify";
import { FileCache } from "./fileCache.js";
import { MergedGamelistSource, MergedLuaSource, looksLikeLua } from "./merged.js";
import { ProviderCooldown } from "./merged.js";
import { UpstreamError, upstreamError } from "./upstream.js";
import { fetchWithDeadline, type Deadline } from "./http.js";
import { requireZip } from "./packageCheck.js";
import { registerDepotRoutes, type DepotUpstreams } from "./routes/depot.js";
import { loadConfig } from "./config.js";
import type { RawStream } from "./depotbox.js";

const log = { info() {}, warn() {}, error() {} } as unknown as FastifyBaseLogger;
const TEST: Deadline = { label: "Test", error: upstreamError };
const ZIP = Buffer.concat([Buffer.from([0x50, 0x4b, 0x03, 0x04]), Buffer.from("rest of the archive")]);

function raw(body: Buffer | string, contentType: string, chunkSize = 2): RawStream {
  const bytes = Buffer.from(body);
  const chunks: Buffer[] = [];
  for (let offset = 0; offset < bytes.length; offset += chunkSize) chunks.push(bytes.subarray(offset, offset + chunkSize));
  return { stream: Readable.from(chunks), contentLength: bytes.length, contentType };
}

async function collect(stream: Readable): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk as Uint8Array));
  return Buffer.concat(chunks);
}

async function serve(listener: RequestListener): Promise<{ server: Server; base: string }> {
  const server = createServer(listener);
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  return { server, base: `http://127.0.0.1:${address.port}` };
}

async function stop(...servers: Server[]): Promise<void> {
  for (const server of servers) {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

test("a Lua is recognised anywhere in the body, but an error page is not", () => {
  const header = "-- Downloaded using DepotBox - https://depotbox.org/\n".repeat(8);
  assert.ok(header.length > 256);
  assert.equal(looksLikeLua(`${header}addappid(70, 1, "00")\n`), true);
  assert.equal(looksLikeLua("setManifestid(1, \"2\")"), true);
  assert.equal(looksLikeLua("<html><body>addappid(70)</body></html>"), false);
  assert.equal(looksLikeLua('{"error":"addappid(70)"}'), false);
  assert.equal(looksLikeLua("print('addappid(70)')"), false);
});

test("a provider without tags keeps the tags a lower-priority provider had", async () => {
  const source = new MergedGamelistSource(
    [
      { name: "depotbox", source: { fetchGamelist: async () => '{"success":true,"games":[{"appid":1,"name":"Game"}]}' } },
      { name: "steamtools", source: { fetchGamelist: async () => '{"games":[{"appid":1,"name":"Old","tags":["nsfw:gore"]},{"appid":2,"name":"Other"}]}' } },
    ],
    log,
  );
  const games = await source.fetchMerged();
  assert.deepEqual(games.find((game) => game.appid === 1), { appid: 1, name: "Game", tags: ["nsfw:gore"] });
  assert.equal(games.length, 2);

  const failing = new MergedGamelistSource(
    [{ name: "depotbox", source: { fetchGamelist: async () => '{"success":false,"games":[]}' } }],
    log,
  );
  await assert.rejects(failing.fetchMerged());
});

test("a rate-limited provider is skipped until its limit resets", async () => {
  const calls = { busy: 0, spare: 0 };
  const lua = new MergedLuaSource(
    [
      {
        name: "busy",
        source: {
          fetchLua: async () => {
            calls.busy += 1;
            throw new UpstreamError(429, "rate limited", 60);
          },
        },
      },
      {
        name: "spare",
        source: {
          fetchLua: async (appid) => {
            calls.spare += 1;
            return { appid, body: `addappid(${appid})`, contentType: "text/plain" };
          },
        },
      },
    ],
    log,
    new ProviderCooldown(),
  );
  await lua.fetchLua("70");
  await lua.fetchLua("70");
  assert.deepEqual(calls, { busy: 1, spare: 2 });
});

test("a missing Lua does not hide a real upstream error", async () => {
  const lua = new MergedLuaSource(
    [
      { name: "down", source: { fetchLua: async () => { throw new UpstreamError(503, "down"); } } },
      { name: "empty", source: { fetchLua: async (appid) => ({ appid, body: "-- nothing here", contentType: "text/plain" }) } },
    ],
    log,
  );
  await assert.rejects(lua.fetchLua("70"), (error) => error instanceof UpstreamError && error.status === 503);
});

test("only a real ZIP archive passes the package check, whole and within its size", async () => {
  assert.deepEqual(await collect(await requireZip(raw(ZIP, "application/zip"), "fixture")), ZIP);
  assert.deepEqual(await collect(await requireZip(raw(ZIP, "application/octet-stream", 1), "fixture")), ZIP);
  const refused = (error: unknown) => error instanceof UpstreamError && error.status === 502;
  await assert.rejects(requireZip(raw("<html>Just a moment...</html>", "text/html"), "fixture"), refused);
  await assert.rejects(requireZip(raw('{"success":false}', "application/json"), "fixture"), refused);
  await assert.rejects(requireZip(raw("not a zip at all", "application/zip"), "fixture"), refused);
  await assert.rejects(requireZip(raw("", "application/zip"), "fixture"), refused);
  const oversized = await requireZip({ ...raw(ZIP, "application/zip"), contentLength: null }, "fixture", 8);
  await assert.rejects(collect(oversized), refused);
});

test("the depot route moves past a provider that answers with a web page", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-depot-"));
  const app = Fastify();
  try {
    const upstreams = {
      depotbox: { downloadDepotPackage: async () => raw("<html>maintenance</html>", "text/html") },
      steamtools: { downloadManifestZip: async () => raw(ZIP, "application/zip") },
      ryu: { downloadDepotPackage: async () => raw(ZIP, "application/zip") },
    } as unknown as DepotUpstreams;
    const cache = new FileCache(dir, 60_000, log);
    registerDepotRoutes(app, upstreams, ["depotbox", "steamtools"], cache, async () => {}, new ProviderCooldown());
    const response = await app.inject({ url: "/v1/depot/package/70" });
    assert.equal(response.statusCode, 200);
    assert.equal(response.headers["x-depot-source"], "steamtools");
    assert.deepEqual(response.rawPayload, ZIP);
  } finally {
    await app.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("credentials never follow a redirect to another host", async () => {
  const seen: IncomingHttpHeaders[] = [];
  const other = await serve((req, res) => {
    seen.push(req.headers);
    res.end("ok");
  });
  const origin = await serve((req, res) => {
    if (req.url === "/away") {
      res.writeHead(302, { location: `${other.base}/landed` });
      res.end();
    } else if (req.url === "/here") {
      res.writeHead(302, { location: "/landed" });
      res.end();
    } else {
      seen.push(req.headers);
      res.end("ok");
    }
  });
  try {
    const headers = { "x-api-key": "secret", "user-agent": "drydock-proxy/1.0" };
    assert.equal(await (await fetchWithDeadline(`${origin.base}/away`, headers, 5000, TEST)).text(), "ok");
    assert.equal(seen[0]?.["x-api-key"], undefined, "the key must not reach another host");
    assert.equal(seen[0]?.["user-agent"], "drydock-proxy/1.0");
    assert.equal(await (await fetchWithDeadline(`${origin.base}/here`, headers, 5000, TEST)).text(), "ok");
    assert.equal(seen[1]?.["x-api-key"], "secret", "a same-origin redirect keeps it");
  } finally {
    await stop(origin.server, other.server);
  }
});

test("a 429 carries the wait the provider asked for", async () => {
  const limited = await serve((_req, res) => {
    res.writeHead(429, { "retry-after": "42" });
    res.end("slow down");
  });
  try {
    await assert.rejects(
      fetchWithDeadline(limited.base, {}, 5000, TEST),
      (error) => error instanceof UpstreamError && error.status === 429 && error.retryAfterSeconds === 42,
    );
  } finally {
    await stop(limited.server);
  }
});

test("an enabled provider cannot boot with the placeholder credential", () => {
  const names = ["PROVIDER_SOURCES", "DEPOTBOX_API_KEY", "DRYDOCK_HMAC_SECRET"] as const;
  const previous = Object.fromEntries(names.map((name) => [name, process.env[name]]));
  try {
    process.env.PROVIDER_SOURCES = "depotbox";
    process.env.DRYDOCK_HMAC_SECRET = "fixture";
    process.env.DEPOTBOX_API_KEY = "your_depotbox_api_key";
    assert.throws(loadConfig, /placeholder/);
    process.env.DEPOTBOX_API_KEY = "a-real-looking-key";
    assert.equal(loadConfig().depotboxApiKey, "a-real-looking-key");
  } finally {
    for (const name of names) {
      if (previous[name] === undefined) delete process.env[name];
      else process.env[name] = previous[name];
    }
  }
});

test("a streamed depot package body is fully cached", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-cache-"));
  try {
    const cache = new FileCache(dir, 60_000, log);
    const stream = await requireZip(raw(ZIP, "application/zip", 3), "fixture");
    const hit = await cache.put("depot:70", stream, "application/zip");
    const sink: Buffer[] = [];
    await pipeline(
      hit.stream(),
      new Writable({
        write(chunk: Buffer, _encoding, done) {
          sink.push(chunk);
          done();
        },
      }),
    );
    assert.deepEqual(Buffer.concat(sink), ZIP);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
