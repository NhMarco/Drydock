// Client for the Ryu generator API (generator.ryuu.lol). The private reseller auth code lives only
// here, server-side, and is never exposed to proxy clients. Ryu's `secure_download` returns the same
// per-app depot package shape Drydock already consumes: a ZIP of `<depot>_<manifestid>.manifest` files
// plus an `<appid>.lua` that carries the depot keys (`addappid(<depot>, 0|1, "<hex>")`).
//
// Endpoints (for reference; only the depot package is wired today):
//   GET /secure_download?appid=<id>&auth_code=<code>       -> application/zip (manifests + lua)
//   GET /resellerlua?appid=<id>&auth_code=<code>           -> text/plain lua
//   GET /files/games.json                                  -> public games list (no auth)
//   GET /files/fixes.json                                  -> public fixes list (no auth)
//   GET /fixes/<name>.zip?auth_code=<code>                 -> fix archive

import type { Config } from "./config.js";
import { UpstreamError, upstreamError, type LuaResult } from "./upstream.js";
import type { GamelistSource, LuaSource, RawStream } from "./depotbox.js";
import { type Deadline, fetchWithDeadline, readBody, streamBody } from "./http.js";

const RYU: Deadline = { label: "Ryu", error: upstreamError };

export class RyuClient implements GamelistSource, LuaSource {
  constructor(private readonly config: Config) {}

  private headers(): Record<string, string> {
    return {
      "user-agent": "drydock-proxy/1.0",
      accept: "*/*",
    };
  }

  private fetchWithTimeout(url: string, timeoutMs: number): Promise<Response> {
    return fetchWithDeadline(url, this.headers(), timeoutMs, RYU);
  }

  // The public games list (no auth). Ryu returns a bare array of `{ appid, name, tags, … }` with the
  // App ID as a string; this normalises it to the `{ games: [{ appid: number, name, tags }] }` shape
  // the merged gamelist and the Rust client expect (`GameDto.appid` is a `u32`).
  async fetchGamelist(): Promise<string> {
    const url = `${this.config.ryuBase}/files/games.json`;
    // The deadline also covers the multi-megabyte body, so it is the gamelist one like the other sources'.
    const response = await this.fetchWithTimeout(url, this.config.gamelistTimeoutMs);
    if (!response.ok) {
      throw new UpstreamError(response.status, `Ryu gamelist returned ${response.status}.`);
    }
    const raw: unknown = await readBody(response.json(), RYU);
    if (!Array.isArray(raw)) throw new UpstreamError(502, "Invalid Ryu gamelist schema.");
    const list = raw;
    const games = list
      .map((entry) => {
        const game = entry as { appid?: unknown; name?: unknown; tags?: unknown };
        const appid = Number.parseInt(String(game.appid ?? ""), 10);
        if (!Number.isInteger(appid) || appid <= 0) return null;
        return {
          appid,
          name: String(game.name ?? ""),
          tags: Array.isArray(game.tags) ? game.tags : [],
        };
      })
      .filter((game): game is { appid: number; name: string; tags: unknown[] } => game !== null);
    return JSON.stringify({ games });
  }

  // The per-app unlock Lua (`addappid(…)` / `setManifestid(…)`), text/plain. Auth code as a query
  // parameter, per Ryu's API.
  async fetchLua(appid: string): Promise<LuaResult> {
    if (!this.config.ryuAuthCode) {
      throw new UpstreamError(502, "Ryu auth code is not configured (set RYU_AUTH_CODE).");
    }
    const url =
      `${this.config.ryuBase}/resellerlua` +
      `?appid=${encodeURIComponent(appid)}&auth_code=${encodeURIComponent(this.config.ryuAuthCode)}`;
    const response = await this.fetchWithTimeout(url, this.config.upstreamTimeoutMs);
    if (!response.ok) {
      throw new UpstreamError(response.status, `Ryu lua returned ${response.status} for AppID ${appid}.`);
    }
    const contentType = response.headers.get("content-type") ?? "text/plain; charset=utf-8";
    return { appid, body: await readBody(response.text(), RYU), contentType };
  }

  // Opens a streaming download of the per-app depot package (a ZIP of the depot manifests plus the
  // `.lua` carrying the depot keys). The auth code is passed as a query parameter, per Ryu's API.
  async downloadDepotPackage(appid: string): Promise<RawStream> {
    if (!this.config.ryuAuthCode) {
      throw new UpstreamError(502, "Ryu auth code is not configured (set RYU_AUTH_CODE).");
    }
    const url =
      `${this.config.ryuBase}/secure_download` +
      `?appid=${encodeURIComponent(appid)}&auth_code=${encodeURIComponent(this.config.ryuAuthCode)}`;
    const response = await this.fetchWithTimeout(url, this.config.depotPackageTimeoutMs);
    if (!response.ok || !response.body) {
      throw new UpstreamError(
        response.ok ? 502 : response.status,
        `Ryu depot package returned ${response.status} for AppID ${appid}.`,
      );
    }
    const lengthHeader = response.headers.get("content-length");
    return {
      stream: streamBody(response.body, RYU),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/zip",
    };
  }
}
