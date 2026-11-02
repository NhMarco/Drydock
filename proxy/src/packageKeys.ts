// Finds the depot keys in a depot package that do not decrypt their own manifest, and replaces them
// with a key another provider has that does.
//
// Providers now and then ship a wrong key for one depot of an app — most often the key of the line
// above, copied down. The manifest beside it gives that away: Steam encrypts a manifest's file names
// with its depot's key, so a key that will not decrypt them is wrong for certain. Drydock decrypts
// those names before it downloads or verifies anything, and one such depot used to fail the whole
// package ("AES decryption failed: CBC/PKCS7 unpad"); Steam cannot use that depot either.
//
// A depot whose key does not fit is looked up in the other providers' Luas, and the first key that
// decrypts its manifest replaces the wrong one in the package's Lua. A depot no provider has a
// fitting key for is left as it is: the app skips that depot and says so.

import { createDecipheriv } from "node:crypto";
import { readZipEntries, rewriteLuaEntries } from "./zipLua.js";

const PAYLOAD_MAGIC = 0x71f617d0;
const METADATA_MAGIC = 0x1f4812be;
const SIGNATURE_MAGIC = 0x1b81b817;
const END_MAGIC = 0x32c415ab;

/**
 * How many file names of a manifest a key has to decrypt to count as its key. A wrong key gets one
 * name past the padding check about once in 256 tries; several, practically never.
 */
const NAMES_CHECKED = 4;

/** Reads the few protobuf fields this needs. Throws on anything malformed. */
class ProtoReader {
  private offset = 0;

  constructor(private readonly bytes: Buffer) {}

  /** The next field number and wire type, or null at the end. */
  next(): { field: number; wire: number } | null {
    if (this.offset >= this.bytes.length) return null;
    const tag = this.varint();
    return { field: Math.floor(tag / 8), wire: tag % 8 };
  }

  varint(): number {
    let value = 0;
    let scale = 1;
    for (let index = 0; index < 10; index += 1) {
      if (this.offset >= this.bytes.length) throw new Error("truncated varint");
      const byte = this.bytes.readUInt8(this.offset++);
      value += (byte & 0x7f) * scale;
      if ((byte & 0x80) === 0) return value;
      scale *= 128;
    }
    throw new Error("varint too long");
  }

  lengthDelimited(): Buffer {
    const length = this.varint();
    if (this.offset + length > this.bytes.length) throw new Error("truncated field");
    const value = this.bytes.subarray(this.offset, this.offset + length);
    this.offset += length;
    return value;
  }

  skip(wire: number): void {
    if (wire === 0) this.varint();
    else if (wire === 1) this.offset += 8;
    else if (wire === 2) this.lengthDelimited();
    else if (wire === 5) this.offset += 4;
    else throw new Error(`unsupported wire type ${wire}`);
  }
}

/**
 * A manifest's depot and the first few of its encrypted file names (none when its names are plain),
 * or null when it is not a manifest this reads.
 */
export function manifestNames(bytes: Buffer): { depot: number; names: string[] } | null {
  try {
    let payload: Buffer | null = null;
    let metadata: Buffer | null = null;
    let offset = 0;
    for (;;) {
      if (offset + 4 > bytes.length) return null;
      const magic = bytes.readUInt32LE(offset);
      offset += 4;
      if (magic === END_MAGIC) break;
      if (magic !== PAYLOAD_MAGIC && magic !== METADATA_MAGIC && magic !== SIGNATURE_MAGIC) return null;
      if (offset + 4 > bytes.length) return null;
      const length = bytes.readUInt32LE(offset);
      offset += 4;
      if (offset + length > bytes.length) return null;
      const section = bytes.subarray(offset, offset + length);
      offset += length;
      if (magic === PAYLOAD_MAGIC) payload = section;
      else if (magic === METADATA_MAGIC) metadata = section;
    }
    if (!payload || !metadata) return null;

    let depot = 0;
    let encrypted = false;
    const meta = new ProtoReader(metadata);
    for (let field = meta.next(); field; field = meta.next()) {
      if (field.field === 1 && field.wire === 0) depot = meta.varint();
      else if (field.field === 4 && field.wire === 0) encrypted = meta.varint() !== 0;
      else meta.skip(field.wire);
    }
    if (!encrypted) return { depot, names: [] };

    const names: string[] = [];
    const files = new ProtoReader(payload);
    for (let field = files.next(); field && names.length < NAMES_CHECKED; field = files.next()) {
      if (field.field !== 1 || field.wire !== 2) {
        files.skip(field.wire);
        continue;
      }
      const mapping = new ProtoReader(files.lengthDelimited());
      for (let inner = mapping.next(); inner; inner = mapping.next()) {
        if (inner.field === 1 && inner.wire === 2) {
          const name = mapping.lengthDelimited().toString("utf8");
          if (name.trim()) names.push(name);
          break;
        }
        mapping.skip(inner.wire);
      }
    }
    return { depot, names };
  } catch {
    return null;
  }
}

/** True when `key` decrypts the encrypted file name `encoded` the way Steam encrypted it. */
function decryptsName(key: Buffer, encoded: string): boolean {
  const data = Buffer.from(encoded.replace(/\s/g, ""), "base64");
  if (data.length < 32 || data.length % 16 !== 0) return false;
  try {
    // The first block is the IV, encrypted on its own (ECB); the rest is CBC with PKCS7 padding.
    const ecb = createDecipheriv("aes-256-ecb", key, null).setAutoPadding(false);
    const iv = Buffer.concat([ecb.update(data.subarray(0, 16)), ecb.final()]);
    const cbc = createDecipheriv("aes-256-cbc", key, iv);
    const plain = Buffer.concat([cbc.update(data.subarray(16)), cbc.final()]);
    const end = plain.indexOf(0);
    new TextDecoder("utf-8", { fatal: true }).decode(end < 0 ? plain : plain.subarray(0, end));
    return true;
  } catch {
    return false;
  }
}

/** True when the hex `key` decrypts every one of `names`. */
export function keyFits(key: string, names: string[]): boolean {
  const bytes = Buffer.from(key, "hex");
  return bytes.length === 32 && names.every((name) => decryptsName(bytes, name));
}

/**
 * Each `addappid(<depot>, 1, "<64 hex>")` line of a Lua as depot, key and line index — read the way
 * the app reads them (drydock-core `DepotKeys::parse_lua`).
 */
function keyLines(lua: string): { depot: number; key: string; line: number }[] {
  const found: { depot: number; key: string; line: number }[] = [];
  lua.split("\n").forEach((raw, line) => {
    const text = raw.trim();
    if (!text.startsWith("addappid(")) return;
    const args = (text.slice("addappid(".length).split(")")[0] ?? "").split(",");
    const depot = args[0]?.trim() ?? "";
    const key = args[2]?.trim().replace(/^"+|"+$/g, "") ?? "";
    if (/^[0-9]{1,10}$/.test(depot) && /^[0-9a-fA-F]{64}$/.test(key)) {
      found.push({ depot: Number(depot), key, line });
    }
  });
  return found;
}

/** The depot keys a Lua sets; the last line for a depot wins, as it does in the app. */
export function luaKeys(lua: string): Map<number, string> {
  return new Map(keyLines(lua).map(({ depot, key }) => [depot, key.toLowerCase()]));
}

/** `lua` with the keys in `fixes` in place of the ones it has, and appended for depots it lacks. */
function patchLua(lua: string, fixes: Map<number, string>, append: number[]): string {
  const lines = lua.split("\n");
  for (const { depot, key, line } of keyLines(lua)) {
    const fixed = fixes.get(depot);
    if (fixed) lines[line] = (lines[line] ?? "").replace(key, fixed);
  }
  const added = append.map((depot) => `addappid(${depot}, 1, "${fixes.get(depot)}")`);
  if (added.length === 0) return lines.join("\n");
  const body = lines.join("\n");
  return `${body}${body.endsWith("\n") || body === "" ? "" : "\n"}${added.join("\n")}\n`;
}

/** A Lua from another provider, to look a depot's key up in. */
export interface AlternativeLua {
  source: string;
  lua: string;
}

export interface KeyRepair {
  /** The package with its Lua corrected, or null when nothing was changed. */
  zip: Buffer | null;
  /** Depots whose key was replaced, with the provider the right one came from. */
  repaired: { depot: number; source: string }[];
  /** Depots whose key does not decrypt their manifest, and for which no provider had one that does. */
  unrepaired: number[];
}

/**
 * Checks every encrypted manifest in `zip` against its depot's key in the package's Lua, and replaces
 * a key that does not fit with one from `alternatives` that does. `alternatives` is only called when
 * some key is wrong or missing.
 */
export async function repairPackageKeys(
  zip: Buffer,
  alternatives: () => Promise<AlternativeLua[]>,
): Promise<KeyRepair> {
  const unchanged: KeyRepair = { zip: null, repaired: [], unrepaired: [] };
  const entries = readZipEntries(zip);
  if (!entries) return unchanged;

  // The first Lua's key for a depot wins over a later Lua's, as in the app.
  const keys = new Map<number, string>();
  let hasLua = false;
  const names = new Map<number, string[]>();
  for (const { name, bytes } of entries) {
    if (!bytes) continue;
    const lower = name.toLowerCase();
    if (lower.endsWith(".lua")) {
      hasLua = true;
      for (const [depot, key] of luaKeys(bytes.toString("utf8"))) if (!keys.has(depot)) keys.set(depot, key);
    } else if (lower.endsWith(".manifest")) {
      const manifest = manifestNames(bytes);
      if (manifest && manifest.names.length > 0) {
        names.set(manifest.depot, [...(names.get(manifest.depot) ?? []), ...manifest.names]);
      }
    }
  }
  const wrong = [...names.keys()].filter((depot) => {
    const key = keys.get(depot);
    return key === undefined || !keyFits(key, names.get(depot) ?? []);
  });
  if (wrong.length === 0) return unchanged;
  if (!hasLua) return { ...unchanged, unrepaired: wrong };

  let others: AlternativeLua[] = [];
  try {
    others = await alternatives();
  } catch {
    // No other provider to ask is the same as none having the key.
  }
  const fixes = new Map<number, string>();
  const repaired: KeyRepair["repaired"] = [];
  for (const depot of wrong) {
    for (const { source, lua } of others) {
      const key = luaKeys(lua).get(depot);
      if (key && key !== keys.get(depot) && keyFits(key, names.get(depot) ?? [])) {
        fixes.set(depot, key);
        repaired.push({ depot, source });
        break;
      }
    }
  }
  const unrepaired = wrong.filter((depot) => !fixes.has(depot));
  if (fixes.size === 0) return { ...unchanged, unrepaired };

  // A depot the package had no key for at all gets its line in the first Lua.
  let append = [...fixes.keys()].filter((depot) => !keys.has(depot));
  const patched = rewriteLuaEntries(zip, (_name, lua) => {
    const result = patchLua(lua, fixes, append);
    append = [];
    return result;
  });
  if (!patched) return { ...unchanged, unrepaired: wrong };
  return { zip: patched, repaired, unrepaired };
}
