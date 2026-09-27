// Removes the providers' comments from an unlock Lua and puts Drydock's own header on top.
//
// Every provider decorates its Luas differently: banners, ads for its site or Discord, generation
// timestamps, commented-out `setManifestid` lines, a `--Mainappid` after a key. None of it does
// anything — Steam executes the code — and all of it tells the user where the file came from rather
// than who served it. What a Lua *does* is untouched: only comments go, found by a small Lua lexer so
// that `--` inside a string is never mistaken for one.
//
// Run `sanitizeLua` (luaSanitize.ts) first: it reads the commented-out `setManifestid` lines to
// recognise depots, and those lines are gone afterwards.
//
// The result is deterministic (no timestamp) and cleaning it again changes nothing — the header is a
// comment too, so it is stripped and written back identically. That matters because a cached package
// may be cleaned a second time, and because the client compares a stored Lua with a fresh one.

import { rewriteLuaEntries } from "./zipLua.js";

/** The first lines of every Lua the proxy serves. */
export function luaHeader(appid: string | null): string[] {
  return [
    appid ? `-- Drydock unlock | App ${appid}` : "-- Drydock unlock",
    "-- https://github.com/NhMarco/Drydock",
  ];
}

/**
 * The level of a Lua long bracket opening at `index` (`[[` is 0, `[==[` is 2), or -1 when there is
 * none there.
 */
function longBracketLevel(source: string, index: number): number {
  if (source[index] !== "[") return -1;
  let cursor = index + 1;
  while (source[cursor] === "=") cursor += 1;
  return source[cursor] === "[" ? cursor - index - 1 : -1;
}

/** Where the long bracket of `level` that opened before `from` closes (just past it), or the end. */
function longBracketEnd(source: string, from: number, level: number): number {
  const close = `]${"=".repeat(level)}]`;
  const end = source.indexOf(close, from);
  return end < 0 ? source.length : end + close.length;
}

/** `source` without its comments; strings and long strings are copied as they are. */
export function stripLuaComments(source: string): string {
  let out = "";
  let index = 0;
  while (index < source.length) {
    const char = source[index];
    if (char === "-" && source[index + 1] === "-") {
      const level = longBracketLevel(source, index + 2);
      if (level >= 0) {
        // --[[ … ]] and --[==[ … ]==]: a block comment, possibly in the middle of a line.
        index = longBracketEnd(source, index + 2 + level + 2, level);
      } else {
        // -- …: to the end of the line (the line break itself is kept).
        const newline = source.indexOf("\n", index);
        index = newline < 0 ? source.length : newline;
      }
      continue;
    }
    if (char === '"' || char === "'") {
      // A quoted string, with its escapes, up to the closing quote or the end of the line.
      let end = index + 1;
      while (end < source.length && source[end] !== char && source[end] !== "\n") {
        end += source[end] === "\\" ? 2 : 1;
      }
      end = Math.min(end + 1, source.length);
      out += source.slice(index, end);
      index = end;
      continue;
    }
    if (char === "[") {
      const level = longBracketLevel(source, index);
      if (level >= 0) {
        const end = longBracketEnd(source, index + level + 2, level);
        out += source.slice(index, end);
        index = end;
        continue;
      }
    }
    out += char;
    index += 1;
  }
  return out;
}

/**
 * The Lua as the proxy serves it: no provider comments, no empty lines, and Drydock's header on top.
 * `appid` names the app in the header when it is known. (Unlock Luas are a list of calls; were one
 * to carry a multi-line long string, its empty lines would go too.)
 */
export function cleanLua(source: string, appid: string | null): string {
  const code = stripLuaComments(source.replace(/^﻿/, ""))
    .split(/\r?\n/)
    .map((line) => line.trimEnd())
    .filter((line) => line.trim().length > 0);
  return `${[...luaHeader(appid), ...code].join("\n")}\n`;
}

/**
 * A depot package with its Luas cleaned (see {@link cleanLua}), each headed with the App ID from its
 * own file name (`<appid>.lua`) or else the package's. The package as it was when it holds nothing
 * to clean or cannot be rewritten safely.
 */
export function cleanPackageLuas(zip: Buffer, appid: string): Buffer {
  const rewritten = rewriteLuaEntries(zip, (name, lua) => {
    const own = /(?:^|\/)(\d+)\.lua$/i.exec(name)?.[1];
    return cleanLua(lua, own ?? appid);
  });
  return rewritten ?? zip;
}
