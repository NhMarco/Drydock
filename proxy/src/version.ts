// Parses the Steam Service version file, tolerating plain text, a JSON string, a JSON number,
// or a JSON object with a "version" field — mirroring the Rust `mfb::parse_version`.

import { GitHubError } from "./github.js";

export function parseServiceVersion(raw: string): string {
  let value = raw.trim().replace(/^﻿/, "").trim();
  if (!value) throw new GitHubError(502, "The Steam Service version file is empty.");
  try {
    const parsed = JSON.parse(value) as unknown;
    if (typeof parsed === "string") value = parsed;
    else if (typeof parsed === "number") value = String(parsed);
    else if (parsed && typeof parsed === "object" && "version" in parsed) {
      value = String((parsed as { version: unknown }).version);
    }
  } catch {
    // Plain text despite the .json extension is allowed.
  }
  value = value.trim().replace(/^"+|"+$/g, "").trim();
  if (!value || value.length > 64) throw new GitHubError(502, "The Steam Service version file is invalid.");
  return value;
}
