// Client for the DepotBox API (https://depotbox.org), an upstream for the gamelist, per-app Lua and
// depot packages. The private API key lives only here, server-side, and is never exposed to proxy
// clients. Errors are surfaced as UpstreamError so the routes can map them uniformly.
//
// Endpoints used (checked against the live API):
//   GET /api/gamelist/games             -> { success, total, games: [{ appid, name }] }  (no tags)
//   GET /api/direct-lua?appid=<id>      -> text/x-lua, generated on demand (can take minutes)
//   GET /api/direct-download?appid=<id> -> application/zip: <appid>.lua (keyed addappid lines) plus
//                                          <depot>_<manifest>.manifest files

import type { Readable } from "node:stream";
import type { Config } from "./config.js";
import { type Deadline, fetchWithDeadline, readBody, streamBody } from "./http.js";
import { UpstreamError, upstreamError, type LuaResult } from "./upstream.js";

const DEPOTBOX: Deadline = { label: "DepotBox", error: upstreamError };

export interface RawStream {
  stream: Readable;
  contentLength: number | null;
  contentType: string;
}

// The gamelist cache and lua route depend only on these narrow shapes, so either upstream can
// back them.
export interface GamelistSource {
  fetchGamelist(): Promise<string>;
}
export interface LuaSource {
  fetchLua(appid: string): Promise<LuaResult>;
}

export class DepotBoxClient implements GamelistSource, LuaSource {
  constructor(private readonly config: Config) {}

  private headers(): Record<string, string> {
    return {
      "x-api-key": this.config.depotboxApiKey,
      "user-agent": "drydock-proxy/1.0",
      accept: "*/*",
    };
  }

  private fetchWithTimeout(url: string, timeoutMs: number): Promise<Response> {
    return fetchWithDeadline(url, this.headers(), timeoutMs, DEPOTBOX);
  }

  // The full catalog as text ({ games: [{ appid, name }] }). Called only by the background refresher.
  async fetchGamelist(): Promise<string> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/gamelist/games`,
      this.config.gamelistTimeoutMs,
    );
    if (!response.ok) throw new UpstreamError(response.status, `Gamelist upstream returned ${response.status}.`);
    return await readBody(response.text(), DEPOTBOX);
  }

  async fetchLua(appid: string): Promise<LuaResult> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/direct-lua?appid=${encodeURIComponent(appid)}`,
      // DepotBox generates the Lua on demand, which takes far longer than the other upstreams need.
      this.config.depotboxLuaTimeoutMs,
    );
    if (!response.ok) {
      throw new UpstreamError(response.status, `Lua upstream returned ${response.status} for AppID ${appid}.`);
    }
    const contentType = response.headers.get("content-type") ?? "text/plain; charset=utf-8";
    return { appid, body: await readBody(response.text(), DEPOTBOX), contentType };
  }

  // Opens a streaming download of the per-app depot package (a ZIP containing the depot manifests
  // and the depot key), from DepotBox's `direct-download`. Drydock uses it to download the real game
  // files (manifest + key -> Steam CDN).
  async downloadDepotPackage(appid: string): Promise<RawStream> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/direct-download?appid=${encodeURIComponent(appid)}`,
      this.config.depotPackageTimeoutMs,
    );
    if (!response.ok || !response.body) {
      throw new UpstreamError(
        response.ok ? 502 : response.status,
        `Depot package upstream returned ${response.status} for AppID ${appid}.`,
      );
    }
    const lengthHeader = response.headers.get("content-length");
    return {
      stream: streamBody(response.body, DEPOTBOX),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/zip",
    };
  }
}
