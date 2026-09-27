// Rewrites the `.lua` entries of a depot package ZIP, leaving every other entry byte for byte as it
// was. Built on node:zlib alone — the proxy has no ZIP dependency and this needs very little of one.
//
// The archive is read through its central directory (the authoritative list, with each entry's
// real sizes and CRC even when the local header defers them to a data descriptor) and written out
// again: untouched entries keep their compressed bytes, a changed Lua is deflated anew. Anything this
// does not understand — ZIP64, an encrypted or oddly compressed Lua, a damaged archive — is left
// alone: the caller then serves the package exactly as the provider sent it, which is what happened
// before this existed.

import { crc32, deflateRawSync, inflateRawSync } from "node:zlib";

const LOCAL_FILE_HEADER = 0x04034b50;
const CENTRAL_DIRECTORY_HEADER = 0x02014b50;
const END_OF_CENTRAL_DIRECTORY = 0x06054b50;
const END_RECORD_SIZE = 22;
const MAXIMUM_COMMENT = 0xffff;

const FLAG_ENCRYPTED = 0x0001;
const FLAG_DATA_DESCRIPTOR = 0x0008;
const METHOD_STORED = 0;
const METHOD_DEFLATED = 8;

interface Entry {
  name: string;
  /** The central directory record as found, patched and written back. */
  record: Buffer;
  /** The local header's extra field, kept as it was. */
  localExtra: Buffer;
  flags: number;
  method: number;
  crc: number;
  compressed: Buffer;
  uncompressedSize: number;
}

function findEndRecord(zip: Buffer): number {
  const lowest = Math.max(0, zip.length - END_RECORD_SIZE - MAXIMUM_COMMENT);
  for (let offset = zip.length - END_RECORD_SIZE; offset >= lowest; offset -= 1) {
    if (zip.readUInt32LE(offset) === END_OF_CENTRAL_DIRECTORY) return offset;
  }
  return -1;
}

/** The entries of `zip` in central-directory order, or null when it is not an archive this reads. */
function readEntries(zip: Buffer): { entries: Entry[]; comment: Buffer } | null {
  if (zip.length < END_RECORD_SIZE) return null;
  const end = findEndRecord(zip);
  if (end < 0) return null;
  const count = zip.readUInt16LE(end + 10);
  const directoryOffset = zip.readUInt32LE(end + 16);
  const commentLength = zip.readUInt16LE(end + 20);
  // ZIP64 markers, or a split archive: not something a provider sends, and not written here.
  if (count === 0xffff || directoryOffset === 0xffffffff) return null;
  if (zip.readUInt16LE(end + 4) !== 0 || zip.readUInt16LE(end + 8) !== count) return null;

  const entries: Entry[] = [];
  let cursor = directoryOffset;
  for (let index = 0; index < count; index += 1) {
    if (cursor + 46 > zip.length || zip.readUInt32LE(cursor) !== CENTRAL_DIRECTORY_HEADER) return null;
    const nameLength = zip.readUInt16LE(cursor + 28);
    const extraLength = zip.readUInt16LE(cursor + 30);
    const entryCommentLength = zip.readUInt16LE(cursor + 32);
    const recordLength = 46 + nameLength + extraLength + entryCommentLength;
    if (cursor + recordLength > zip.length) return null;
    const record = Buffer.from(zip.subarray(cursor, cursor + recordLength));
    const compressedSize = record.readUInt32LE(20);
    const uncompressedSize = record.readUInt32LE(24);
    const localOffset = record.readUInt32LE(42);
    if (compressedSize === 0xffffffff || uncompressedSize === 0xffffffff || localOffset === 0xffffffff) {
      return null;
    }
    if (localOffset + 30 > zip.length || zip.readUInt32LE(localOffset) !== LOCAL_FILE_HEADER) return null;
    const localNameLength = zip.readUInt16LE(localOffset + 26);
    const localExtraLength = zip.readUInt16LE(localOffset + 28);
    const dataStart = localOffset + 30 + localNameLength + localExtraLength;
    if (dataStart + compressedSize > zip.length) return null;
    entries.push({
      name: record.subarray(46, 46 + nameLength).toString("utf8"),
      record,
      localExtra: Buffer.from(
        zip.subarray(localOffset + 30 + localNameLength, localOffset + 30 + localNameLength + localExtraLength),
      ),
      flags: record.readUInt16LE(8),
      method: record.readUInt16LE(10),
      crc: record.readUInt32LE(16),
      compressed: zip.subarray(dataStart, dataStart + compressedSize),
      uncompressedSize,
    });
    cursor += recordLength;
  }
  return { entries, comment: Buffer.from(zip.subarray(end + 22, end + 22 + commentLength)) };
}

/** The bytes of an entry this can read, verified against its CRC, or null. */
function readEntry(entry: Entry): Buffer | null {
  if (entry.flags & FLAG_ENCRYPTED) return null;
  let bytes: Buffer;
  try {
    if (entry.method === METHOD_STORED) bytes = Buffer.from(entry.compressed);
    else if (entry.method === METHOD_DEFLATED) bytes = inflateRawSync(entry.compressed);
    else return null;
  } catch {
    return null;
  }
  if (bytes.length !== entry.uncompressedSize || crc32(bytes) >>> 0 !== entry.crc >>> 0) return null;
  return bytes;
}

/**
 * `zip` with each `.lua` entry replaced by `transform(name, text)`, or null when nothing changed or
 * the archive is not one this can rewrite safely — the caller then keeps the original.
 */
export function rewriteLuaEntries(zip: Buffer, transform: (name: string, lua: string) => string): Buffer | null {
  const archive = readEntries(zip);
  if (!archive) return null;

  let changed = false;
  for (const entry of archive.entries) {
    if (!entry.name.toLowerCase().endsWith(".lua")) continue;
    const bytes = readEntry(entry);
    if (!bytes) continue;
    const rewritten = Buffer.from(transform(entry.name, bytes.toString("utf8")), "utf8");
    if (rewritten.equals(bytes)) continue;
    entry.method = METHOD_DEFLATED;
    entry.compressed = deflateRawSync(rewritten);
    entry.crc = crc32(rewritten) >>> 0;
    entry.uncompressedSize = rewritten.length;
    changed = true;
  }
  if (!changed) return null;

  const parts: Buffer[] = [];
  const records: Buffer[] = [];
  let offset = 0;
  for (const entry of archive.entries) {
    const name = entry.record.subarray(46, 46 + entry.record.readUInt16LE(28));
    // Sizes and CRC go into the local header, so no entry needs a data descriptor any more.
    const flags = entry.flags & ~FLAG_DATA_DESCRIPTOR;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(LOCAL_FILE_HEADER, 0);
    local.writeUInt16LE(entry.record.readUInt16LE(6), 4); // version needed
    local.writeUInt16LE(flags, 6);
    local.writeUInt16LE(entry.method, 8);
    local.writeUInt16LE(entry.record.readUInt16LE(12), 10); // time
    local.writeUInt16LE(entry.record.readUInt16LE(14), 12); // date
    local.writeUInt32LE(entry.crc >>> 0, 14);
    local.writeUInt32LE(entry.compressed.length, 18);
    local.writeUInt32LE(entry.uncompressedSize, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(entry.localExtra.length, 28);

    const record = Buffer.from(entry.record);
    record.writeUInt16LE(flags, 8);
    record.writeUInt16LE(entry.method, 10);
    record.writeUInt32LE(entry.crc >>> 0, 16);
    record.writeUInt32LE(entry.compressed.length, 20);
    record.writeUInt32LE(entry.uncompressedSize, 24);
    record.writeUInt32LE(offset, 42);
    records.push(record);

    parts.push(local, name, entry.localExtra, entry.compressed);
    offset += local.length + name.length + entry.localExtra.length + entry.compressed.length;
  }
  const directory = Buffer.concat(records);
  const end = Buffer.alloc(END_RECORD_SIZE);
  end.writeUInt32LE(END_OF_CENTRAL_DIRECTORY, 0);
  end.writeUInt16LE(archive.entries.length, 8);
  end.writeUInt16LE(archive.entries.length, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(archive.comment.length, 20);
  if (offset > 0xffffffff) return null;
  return Buffer.concat([...parts, directory, end, archive.comment]);
}
