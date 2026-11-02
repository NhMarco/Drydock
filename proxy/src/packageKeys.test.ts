// Run with `npm test` in proxy/.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createCipheriv, randomBytes } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { crc32 } from "node:zlib";
import Fastify, { type FastifyBaseLogger } from "fastify";
import { keyFits, luaKeys, manifestNames, repairPackageKeys } from "./packageKeys.js";
import { readZipEntries } from "./zipLua.js";
import { FileCache } from "./fileCache.js";
import { ProviderCooldown } from "./merged.js";
import { registerDepotRoutes, type DepotUpstreams } from "./routes/depot.js";

const log = { info() {}, warn() {}, error() {} } as unknown as FastifyBaseLogger;

const RIGHT = "11".repeat(32);
const WRONG = "22".repeat(32);
const OTHER = "33".repeat(32);

/** A file name encrypted the way Steam does it: an ECB-encrypted IV, then CBC with PKCS7. */
function encryptName(name: string, key: string): string {
  const keyBytes = Buffer.from(key, "hex");
  const iv = randomBytes(16);
  const ecb = createCipheriv("aes-256-ecb", keyBytes, null).setAutoPadding(false);
  const cbc = createCipheriv("aes-256-cbc", keyBytes, iv);
  return Buffer.concat([
    ecb.update(iv),
    ecb.final(),
    cbc.update(Buffer.from(name)),
    cbc.final(),
  ]).toString("base64");
}

function varint(value: number): Buffer {
  const bytes: number[] = [];
  while (value >= 0x80) {
    bytes.push((value % 0x80) | 0x80);
    value = Math.floor(value / 0x80);
  }
  bytes.push(value);
  return Buffer.from(bytes);
}

function field(number: number, value: Buffer): Buffer {
  return Buffer.concat([varint(number * 8 + 2), varint(value.length), value]);
}

/** A manifest for `depot` whose file names are encrypted with `key` (or plain without one). */
function manifest(depot: number, names: string[], key?: string): Buffer {
  const payload = Buffer.concat(
    names.map((name) => field(1, Buffer.concat([field(1, Buffer.from(key ? encryptName(name, key) : name)), varint(16), varint(5)]))),
  );
  const metadata = Buffer.concat([varint(8), varint(depot), varint(16), varint(42), varint(32), varint(key ? 1 : 0)]);
  const section = (magic: number, body: Buffer) => {
    const head = Buffer.alloc(8);
    head.writeUInt32LE(magic, 0);
    head.writeUInt32LE(body.length, 4);
    return Buffer.concat([head, body]);
  };
  const end = Buffer.alloc(4);
  end.writeUInt32LE(0x32c415ab, 0);
  return Buffer.concat([section(0x71f617d0, payload), section(0x1f4812be, metadata), end]);
}

/** A stored (uncompressed) ZIP of `entries`. */
function zip(entries: Record<string, Buffer | string>): Buffer {
  const parts: Buffer[] = [];
  const directory: Buffer[] = [];
  let offset = 0;
  for (const [entryName, content] of Object.entries(entries)) {
    const data = Buffer.from(content);
    const name = Buffer.from(entryName);
    const crc = crc32(data) >>> 0;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    const record = Buffer.alloc(46);
    record.writeUInt32LE(0x02014b50, 0);
    record.writeUInt16LE(20, 4);
    record.writeUInt16LE(20, 6);
    record.writeUInt32LE(crc, 16);
    record.writeUInt32LE(data.length, 20);
    record.writeUInt32LE(data.length, 24);
    record.writeUInt16LE(name.length, 28);
    record.writeUInt32LE(offset, 42);
    parts.push(local, name, data);
    directory.push(record, name);
    offset += 30 + name.length + data.length;
  }
  const directoryBytes = Buffer.concat(directory);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(directory.length / 2, 8);
  end.writeUInt16LE(directory.length / 2, 10);
  end.writeUInt32LE(directoryBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, directoryBytes, end]);
}

function luaOf(zipBytes: Buffer): string {
  const entry = readZipEntries(zipBytes)?.find(({ name }) => name.endsWith(".lua"));
  assert.ok(entry?.bytes, "the package has a Lua");
  return entry.bytes.toString("utf8");
}

const NAMES = ["Game.exe", "data/level1.pak", "data/level2.pak", "readme.txt", "engine.dll"];

test("a manifest's names are read, and only the right key decrypts them", () => {
  const read = manifestNames(manifest(701, NAMES, RIGHT));
  assert.equal(read?.depot, 701);
  assert.equal(read?.names.length, 4);
  assert.ok(keyFits(RIGHT, read.names));
  assert.ok(!keyFits(WRONG, read.names));
  assert.deepEqual(manifestNames(manifest(702, NAMES)), { depot: 702, names: [] });
  assert.equal(manifestNames(Buffer.from("not a manifest")), null);
});

test("Lua keys are read the way the app reads them", () => {
  const keys = luaKeys(
    ["addappid(700)", `addappid(701, 1, "${RIGHT}")`, `addappid(702,1,"${WRONG}") -- note`, "addappid(703, 1, \"short\")"].join("\n"),
  );
  assert.deepEqual([...keys], [
    [701, RIGHT],
    [702, WRONG],
  ]);
});

test("a package whose keys fit is left alone, without asking anyone", async () => {
  const lua = `addappid(700)\naddappid(701, 1, "${RIGHT}")\n`;
  const result = await repairPackageKeys(zip({ "700.lua": lua, "701_42.manifest": manifest(701, NAMES, RIGHT) }), () => {
    throw new Error("no other provider should be asked");
  });
  assert.deepEqual(result, { zip: null, repaired: [], unrepaired: [] });
});

test("a wrong key is replaced by another provider's key that fits", async () => {
  // The provider copied depot 701's key down onto 702, as observed.
  const lua = `addappid(700)\naddappid(701, 1, "${RIGHT}")\naddappid(702, 1, "${RIGHT}")\nsetManifestid(702, "42")\n`;
  const result = await repairPackageKeys(
    zip({ "700.lua": lua, "701_42.manifest": manifest(701, NAMES, RIGHT), "702_42.manifest": manifest(702, NAMES, OTHER) }),
    async () => [
      { source: "steamtools", lua: `addappid(702, 1, "${WRONG}")` },
      { source: "ryu", lua: `addappid(702, 1, "${OTHER}")` },
    ],
  );
  assert.deepEqual(result.repaired, [{ depot: 702, source: "ryu" }]);
  assert.deepEqual(result.unrepaired, []);
  assert.ok(result.zip);
  assert.equal(
    luaOf(result.zip),
    `addappid(700)\naddappid(701, 1, "${RIGHT}")\naddappid(702, 1, "${OTHER}")\nsetManifestid(702, "42")\n`,
  );
  // The manifests are untouched.
  const manifests = readZipEntries(result.zip)?.filter(({ name }) => name.endsWith(".manifest")) ?? [];
  assert.equal(manifests.length, 2);
});

test("a missing key is added, and one nobody has is reported", async () => {
  const lua = `addappid(700)\naddappid(701, 1, "${WRONG}")`;
  const result = await repairPackageKeys(
    zip({ "700.lua": lua, "701_1.manifest": manifest(701, NAMES, RIGHT), "703_1.manifest": manifest(703, NAMES, OTHER) }),
    async () => [{ source: "ryu", lua: `addappid(701, 1, "${WRONG}")\naddappid(703, 1, "${OTHER}")` }],
  );
  assert.deepEqual(result.repaired, [{ depot: 703, source: "ryu" }]);
  assert.deepEqual(result.unrepaired, [701]);
  assert.ok(result.zip);
  assert.equal(luaOf(result.zip), `addappid(700)\naddappid(701, 1, "${WRONG}")\naddappid(703, 1, "${OTHER}")\n`);
});

test("no fitting key anywhere leaves the package as it is", async () => {
  const lua = `addappid(701, 1, "${WRONG}")\n`;
  const result = await repairPackageKeys(zip({ "701.lua": lua, "701_1.manifest": manifest(701, NAMES, RIGHT) }), async () => {
    throw new Error("provider down");
  });
  assert.deepEqual(result, { zip: null, repaired: [], unrepaired: [701] });
});

test("the depot route serves the package with the key another provider has", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-keys-"));
  const app = Fastify();
  try {
    const provider = zip({
      "701.lua": `addappid(701, 1, "${WRONG}")\n`,
      "701_1.manifest": manifest(701, NAMES, RIGHT),
    });
    const asked: string[] = [];
    const upstreams = {
      depotbox: {
        downloadDepotPackage: async () => ({
          stream: Readable.from([provider]),
          contentLength: provider.length,
          contentType: "application/zip",
        }),
      },
      luas: {
        names: () => ["depotbox", "ryu"],
        fetchLua: async (appid: string, only?: string) => {
          asked.push(only ?? "");
          return { appid, body: `addappid(701, 1, "${RIGHT}")`, contentType: "text/plain" };
        },
      },
    } as unknown as DepotUpstreams;
    registerDepotRoutes(app, upstreams, ["depotbox"], new FileCache(dir, 60_000, log), async () => {}, new ProviderCooldown());

    const response = await app.inject({ url: "/v1/depot/package/701" });
    assert.equal(response.statusCode, 200);
    assert.deepEqual(asked, ["ryu"]);
    assert.deepEqual([...luaKeys(luaOf(response.rawPayload))], [[701, RIGHT]]);
  } finally {
    await app.close();
    await rm(dir, { recursive: true, force: true });
  }
});
