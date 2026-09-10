// Strips broken `addappid` depot lines that arrive from the upstream API without their manifest
// decryption key.
//
// A usable unlock line is one of:
//   addappid(<appid>, 1)            -- the main app or a DLC (no depot key needed)
//   addappid(<depotid>, 1, "<key>") -- a depot together with its manifest decryption key
//
// The upstream occasionally emits a bare, single-argument `addappid(<depotid>)` — a depot with no
// flag and no key. That depot can never be decrypted, and its presence makes the whole unlock fail,
// so we drop exactly those single-argument calls. Everything else (real entries, comments,
// setManifestid, …) is kept untouched.

// A whole line whose only statement is `addappid(<digits>)` (optionally followed by a comment).
const BARE_ADDAPPID_LINE = /^\s*addappid\(\s*\d+\s*\)\s*(?:--.*)?$/;

export interface SanitizedLua {
  body: string;
  removed: number;
}

export function sanitizeLua(body: string): SanitizedLua {
  const lines = body.split(/\r?\n/);
  let removed = 0;
  const kept = lines.filter((line) => {
    if (BARE_ADDAPPID_LINE.test(line)) {
      removed += 1;
      return false;
    }
    return true;
  });
  return { body: kept.join("\n"), removed };
}
