// Run with `npm test` in proxy/.

import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { FastifyBaseLogger } from "fastify";
import { LauncherIndex, launcherOf, parseBlockedLaunchers, type StoreFacts } from "./launcherCheck.js";
import { DenuvoFixesCache, classifyFixFile, resolveFixes, resolveManifests } from "./denuvoFixesCache.js";
import type { Config } from "./config.js";
import type { ContentEntry, GitHubClient } from "./github.js";

const log = { info() {}, warn() {}, error() {} } as unknown as FastifyBaseLogger;

function file(name: string): ContentEntry {
  return { type: "file", name, path: `Files/fix/${name}`, sha: "a".repeat(40), size: 1 } as ContentEntry;
}

test("the store page names the launcher a game needs", () => {
  // As Steam words it for games that have fixes today.
  assert.equal(launcherOf({ publishers: ["Electronic Arts"], ext_user_account_notice: "EA Account " }), "ea");
  assert.equal(launcherOf({ publishers: ["Electronic Arts"] }), "ea", "Dead Space names no account");
  assert.equal(launcherOf({ ext_user_account_notice: "EA Account linking required (Supports Linking to Steam Account)" }), "ea");
  assert.equal(launcherOf({ publishers: ["Ubisoft"], ext_user_account_notice: "Uplay (Supports Linking to Steam Account)" }), "ubisoft");
  assert.equal(launcherOf({ ext_user_account_notice: "Ubisoft Connect launcher (Supports Linking to Steam Account)" }), "ubisoft");
  assert.equal(launcherOf({ publishers: ["Rockstar Games"], ext_user_account_notice: "Rockstar Games (Supports Linking to Steam Account)" }), "rockstar");
  // An account to link is not a launcher.
  assert.equal(launcherOf({ publishers: ["Activision"], ext_user_account_notice: "Activision Account " }), null);
  assert.equal(launcherOf({ publishers: ["PlayStation Publishing LLC"], ext_user_account_notice: "PlayStation Network (Supports Linking to Steam Account)" }), null);
  assert.equal(launcherOf({ publishers: ["CAPCOM Co., Ltd."], ext_user_account_notice: "" }), null);
  // Lookalikes are not EA.
  assert.equal(launcherOf({ publishers: ["Team17"], ext_user_account_notice: "Realm account" }), null);
  assert.equal(launcherOf({ publishers: "Ubisoft" } as StoreFacts), null, "a malformed field is ignored");
});

test("FIX_BLOCKED_LAUNCHERS: unset blocks all, none blocks nothing, a typo stops the start", () => {
  assert.deepEqual(parseBlockedLaunchers(undefined), ["ea", "ubisoft", "rockstar"]);
  assert.deepEqual(parseBlockedLaunchers("none"), []);
  assert.deepEqual(parseBlockedLaunchers(""), []);
  assert.deepEqual(parseBlockedLaunchers(" EA, ubisoft ,ea"), ["ea", "ubisoft"]);
  assert.throws(() => parseBlockedLaunchers("ea,epic"), /epic/);
});

test("a fix game is held back until its store page shows it runs from Steam alone", async () => {
  const directory = await mkdtemp(join(tmpdir(), "launchers-"));
  try {
    const pages: Record<number, StoreFacts | null | Error> = {
      1: { publishers: ["Game Science"] },
      2: { publishers: ["Ubisoft"] },
      3: new Error("Steam is down"),
      4: null, // no store page
    };
    let asked = 0;
    const options = {
      blocked: parseBlockedLaunchers(undefined),
      file: join(directory, "fix-launchers.json"),
      fetchFacts: async (appid: number) => {
        asked += 1;
        const page = pages[appid];
        if (page instanceof Error) throw page;
        return page ?? null;
      },
      log,
      spacingMs: 0,
      retryAfterMs: 60_000,
    };
    const index = new LauncherIndex(options);
    assert.equal(index.allows(1), false, "not known yet");
    await index.check([1, 2, 3, 4]);
    assert.equal(index.allows(1), true);
    assert.equal(index.allows(2), false, "Ubisoft Connect");
    assert.equal(index.allows(3), false, "Steam could not be asked: still held back");
    assert.equal(index.allows(4), true, "nothing on the store page names a launcher");
    assert.equal(asked, 4);

    await index.check([1, 2, 3, 4]);
    assert.equal(asked, 4, "known games are not asked again, a failed one waits before the next try");

    const restarted = new LauncherIndex({ ...options, fetchFacts: async () => assert.fail("asked Steam again") });
    await restarted.check([1, 2]);
    assert.equal(restarted.allows(1), true, "verdicts survive a restart");
    assert.equal(restarted.allows(2), false);

    const unfiltered = new LauncherIndex({ ...options, blocked: [] });
    assert.equal(unfiltered.allows(2), true, "with no launcher blocked every fix is offered");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("the fix folder lists the pinned builds' manifests beside the fixes", () => {
  assert.deepEqual(classifyFixFile("1328671_9466046803274430000.manifest"), { kind: "manifest", depotId: 1328671 });
  assert.equal(classifyFixFile("1328671_0.manifest"), null);
  assert.equal(classifyFixFile("abc_123.manifest"), null);
  const entries = [
    file("1328670.lua"),
    file("1328670.zip.001"),
    file("1328671_9466046803274430000.manifest"),
    file("readme.txt"),
  ];
  assert.deepEqual(resolveFixes(entries).map((fix) => fix.appid), [1328670]);
  assert.deepEqual(resolveManifests(entries).map((manifest) => manifest.name), ["1328671_9466046803274430000.manifest"]);
});

test("held-back fixes are neither listed nor served; manifests are", async () => {
  const entries = [file("10.lua"), file("10.zip"), file("20.lua"), file("20.zip"), file("21_55.manifest")];
  const github = { listDirectory: async () => entries } as unknown as GitHubClient;
  const config = { fixDirectory: "Files/fix", fixesManifestTtlSeconds: 300 } as Config;
  const index = { allows: (appid: number) => appid === 10, check: async () => {} } as unknown as LauncherIndex;
  const cache = new DenuvoFixesCache(config, github, log, index);
  assert.deepEqual((await cache.getFixes()).map((fix) => fix.appid), [10]);
  assert.deepEqual((await cache.getManifests()).map((manifest) => manifest.name), ["21_55.manifest"]);
  assert.ok(await cache.resolveFile("10.zip"));
  assert.equal(await cache.resolveFile("20.zip"), undefined);
  assert.ok(await cache.resolveFile("21_55.manifest"));
});
