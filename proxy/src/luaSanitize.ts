// Strips `addappid` depot lines that arrive from the upstream API without their manifest
// decryption key.
//
// The three shapes that appear in an unlock Lua:
//
//   addappid(<id>)                   -- ownership: the app itself or one of its DLC
//   addappid(<depotid>, <flag>, "…") -- a depot together with its manifest decryption key
//   setManifestid(<depotid>, "…")    -- pins that depot to a specific manifest
//
// The first two are indistinguishable by argument count alone, which is the trap: an ownership line
// and a keyless depot line are both `addappid(<digits>)`. Removing every one of them — as this file
// used to — deletes the app's own ID and all of its DLC, which is the entire point of the unlock.
// Observed upstream output for a single app: 24 lines in, 22 of them "bare", every one an ownership
// declaration and not one a depot.
//
// The distinguishing signal is `setManifestid`: a depot only matters if something is going to
// download it, and anything to be downloaded is pinned to a manifest. So a bare `addappid(N)` is
// only dropped when the same file pins N to a manifest *and* never supplies a key for it — exactly
// the "depot that can never be decrypted" case this guard exists for. Ownership lines carry no
// manifest and are always kept.

const BARE_ADDAPPID = /^\s*addappid\(\s*(\d+)\s*\)\s*(?:--.*)?$/;
/// `addappid(<id>, <flag>, "<key>")` — a depot whose key is present.
const KEYED_ADDAPPID = /^\s*addappid\(\s*(\d+)\s*,[^)]*"[0-9a-fA-F]+"\s*\)/;
/// `setManifestid(<id>, …)`, whether or not the line is commented out.
const SET_MANIFEST_ID = /setManifestid\(\s*(\d+)/;

export interface SanitizedLua {
  body: string;
  removed: number;
}

export function sanitizeLua(body: string): SanitizedLua {
  const lines = body.split(/\r?\n/);

  const pinnedToManifest = new Set<string>();
  const hasKey = new Set<string>();
  for (const line of lines) {
    const pinned = SET_MANIFEST_ID.exec(line)?.[1];
    if (pinned) pinnedToManifest.add(pinned);
    const keyed = KEYED_ADDAPPID.exec(line)?.[1];
    if (keyed) hasKey.add(keyed);
  }

  let removed = 0;
  const kept = lines.filter((line) => {
    const id = BARE_ADDAPPID.exec(line)?.[1];
    if (id === undefined) return true;
    // Undecryptable depot: pinned to a manifest, but no key anywhere in the file.
    if (pinnedToManifest.has(id) && !hasKey.has(id)) {
      removed += 1;
      return false;
    }
    return true;
  });
  return { body: kept.join("\n"), removed };
}
