// Run with `npm test` in proxy/.

import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyBaseLogger } from "fastify";
import { ProviderIndex, buildListing } from "./providerIndex.js";
import { MergedGamelistSource, MergedLuaSource } from "./merged.js";
import { registerSourcesRoute } from "./routes/sources.js";
import { registerLuaRoute } from "./routes/lua.js";
import { UpstreamError } from "./upstream.js";
import type { Config } from "./config.js";

const log = { info() {}, warn() {}, error() {} } as unknown as FastifyBaseLogger;

test("a provider's listing says which apps it has and, where its list dates them, how current", async () => {
  const ryu = buildListing("ryu", [
    { appid: "730", name: "CS2", updated_date: "2026-10-05T12:00:00+00:00" },
    { appid: 10, name: "CS", updated_date: "" },
    { appid: "oops", name: "bad" },
  ])!;
  assert.deepEqual([...ryu.ids], [10, 730], "sorted, malformed rows dropped");
  const depotbox = buildListing("depotbox", [{ appid: 730, name: "CS2" }])!;
  assert.equal(depotbox.updated, null);
  assert.equal(buildListing("steamtools", [{ appid: 730 }]), null, "a list of every Steam app says nothing");

  const directory = await mkdtemp(join(tmpdir(), "provider-index-"));
  try {
    const file = join(directory, "provider-index.json");
    const index = new ProviderIndex(file, log);
    index.set("ryu", ryu);
    index.set("depotbox", depotbox);
    assert.deepEqual(index.lookup("ryu", 730), { listed: true, updated: Date.parse("2026-10-05T12:00:00Z") / 1000 });
    assert.deepEqual(index.lookup("ryu", 10), { listed: true, updated: null });
    assert.deepEqual(index.lookup("ryu", 440), { listed: false, updated: null });
    assert.deepEqual(index.lookup("steamtools", 730), { listed: null, updated: null });
    assert.equal(index.lookup("hubcap", 730), null);
    await index.save();

    const restarted = new ProviderIndex(file, log);
    await restarted.load();
    assert.deepEqual(restarted.lookup("ryu", 730), index.lookup("ryu", 730), "it survives a restart");
    assert.deepEqual(restarted.lookup("depotbox", 10), { listed: false, updated: null });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("the gamelist merge fills the index, and a provider that failed keeps its last listing", async () => {
  const index = new ProviderIndex(null, log);
  index.set("depotbox", { ids: Uint32Array.from([5]), updated: null });
  const merged = new MergedGamelistSource(
    [
      { name: "ryu", source: { fetchGamelist: async () => JSON.stringify([{ appid: 730, name: "CS2", updated_date: "2026-10-05" }]) } },
      { name: "depotbox", source: { fetchGamelist: async () => { throw new UpstreamError(502, "down"); } } },
    ],
    log,
    index,
  );
  // parseGames wants `{ games: [...] }`; Ryu's real list is an array, wrapped by its client.
  (merged as unknown as { sources: { source: { fetchGamelist: () => Promise<string> } }[] }).sources[0]!.source.fetchGamelist =
    async () => JSON.stringify({ games: [{ appid: 730, name: "CS2", updated_date: "2026-10-05" }] });
  await merged.fetchMerged();
  assert.equal(index.lookup("ryu", 730)?.listed, true);
  assert.equal(index.lookup("depotbox", 5)?.listed, true, "the failed provider's listing stays");
});

test("GET /v1/sources/:appid answers in provider order", async () => {
  const index = new ProviderIndex(null, log);
  index.set("ryu", { ids: Uint32Array.from([730]), updated: Uint32Array.from([1_790_000_000]) });
  index.set("depotbox", { ids: Uint32Array.from([10]), updated: null });
  const app = Fastify();
  registerSourcesRoute(app, ["depotbox", "steamtools", "ryu"], index, async () => {});
  try {
    const response = await app.inject({ url: "/v1/sources/730" });
    assert.equal(response.statusCode, 200);
    assert.deepEqual(response.json(), {
      sources: [
        { name: "depotbox", listed: false, updated: null },
        { name: "steamtools", listed: null, updated: null },
        { name: "ryu", listed: true, updated: 1_790_000_000 },
      ],
    });
    assert.equal((await app.inject({ url: "/v1/sources/abc" })).statusCode, 400);
  } finally {
    await app.close();
  }
});

test("a Lua can be asked of one provider alone", async () => {
  const asked: string[] = [];
  const lua = (name: string) => ({
    fetchLua: async (appid: string) => {
      asked.push(name);
      return { body: `addappid(${appid})\n-- from ${name}`, contentType: "text/plain" };
    },
  });
  const source = new MergedLuaSource(
    [
      { name: "depotbox", source: lua("depotbox") },
      { name: "ryu", source: lua("ryu") },
    ],
    log,
  );
  const app = Fastify();
  registerLuaRoute(app, { luaRateMax: 100, luaRateWindowSeconds: 60, luaCacheTtlSeconds: 60 } as Config, source, async () => {});
  try {
    assert.equal((await app.inject({ url: "/v1/lua/730" })).statusCode, 200);
    assert.equal((await app.inject({ url: "/v1/lua/730?source=ryu" })).statusCode, 200);
    assert.deepEqual(asked, ["depotbox", "ryu"], "the picked provider is asked, not the cached automatic answer");
    assert.equal((await app.inject({ url: "/v1/lua/730?source=hubcap" })).statusCode, 400);
  } finally {
    await app.close();
  }
});
