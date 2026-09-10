// Run with `npm test` in proxy/.
//
// These cases are the ones that mattered in practice: the sanitizer used to drop every
// single-argument `addappid`, which deleted each app's own ID and all of its DLC. Real upstream
// output for one title went from 24 lines to 2, and the resulting unlock could not work because
// nothing declared ownership of the app any more.

import assert from "node:assert/strict";
import { test } from "node:test";
import { sanitizeLua } from "./luaSanitize.js";

const lines = (body: string): string[] => body.split(/\r?\n/).filter((line) => line.trim().length > 0);

test("keeps the app's own ID and its DLC", () => {
  // Real upstream output for AppID 3751260.
  const raw = [
    'addappid(3751261,0,"fe44cc16911c3d5dabb2b862a70f1b8b58ee79e1da1f47b8bf03af487c17e8fc")',
    '-- setManifestid(3751261,"6667172545766883229")',
    "addappid(3751260)",
    "addappid(4417540)",
    "addappid(4417550)",
  ].join("\n");

  const result = sanitizeLua(raw);
  assert.equal(result.removed, 0);
  assert.equal(lines(result.body).length, 5);
  assert.match(result.body, /addappid\(3751260\)/, "the app's own ID must survive");
  assert.match(result.body, /addappid\(4417540\)/, "DLC must survive");
});

test("drops a depot that is pinned to a manifest but has no key anywhere", () => {
  const raw = ["addappid(700)", "addappid(701)", 'setManifestid(701,"999")'].join("\n");

  const result = sanitizeLua(raw);
  assert.equal(result.removed, 1);
  assert.doesNotMatch(result.body, /addappid\(701\)/, "the undecryptable depot goes");
  assert.match(result.body, /addappid\(700\)/, "the ownership line stays");
});

test("keeps a pinned depot when its key is supplied", () => {
  const raw = [
    "addappid(800)",
    'addappid(801,0,"abcdef")',
    "addappid(801)",
    'setManifestid(801,"999")',
  ].join("\n");

  const result = sanitizeLua(raw);
  assert.equal(result.removed, 0);
});

test("a commented-out setManifestid still marks the id as a depot", () => {
  // Upstreams emit these commented out; the id is a depot either way.
  const raw = ["addappid(900)", "addappid(901)", '-- setManifestid(901,"5")'].join("\n");

  assert.equal(sanitizeLua(raw).removed, 1);
});

test("leaves an empty body and a comment-only body untouched", () => {
  assert.equal(sanitizeLua("").removed, 0);
  const comments = "-- nothing to see\n-- here";
  assert.equal(sanitizeLua(comments).body, comments);
});
