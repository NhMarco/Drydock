// Client for the DepotBox API (https://depotbox.org), the upstream for the gamelist, per-app
// Lua, and game fixes. The private API key lives only here, server-side, and is never exposed to
// proxy clients. Errors are surfaced as UpstreamError so the routes can map them uniformly.
//
// Endpoints used:
//   GET /api/gamelist/games          -> { success, total, games: [{ appid, name }] }  (no rate limit)
//   GET /api/direct-lua?appid=<id>   -> text/plain lua
//   GET /api/game-fixes              -> { success, count, games: [{ name, fixes: [{ id, ... }] }] }
//   GET /api/game-fixes/download?id= -> the fix archive bytes (RAR), streamed

import { Readable } from "node:stream";
import type { Config } from "./config.js";
import { UpstreamError, type LuaResult } from "./upstream.js";

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

  private async fetchWithTimeout(url: string, timeoutMs: number): Promise<Response> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      return await fetch(url, { headers: this.headers(), signal: controller.signal });
    } catch (error) {
      if (error instanceof Error && error.name === "AbortError") {
        throw new UpstreamError(504, "DepotBox request timed out.");
      }
      throw new UpstreamError(502, "DepotBox request failed.");
    } finally {
      clearTimeout(timer);
    }
  }

  // The full catalog as text ({ games: [{ appid, name }] }). Called only by the background
  // refresher; free upstream (does not count against the rate limit).
  async fetchGamelist(): Promise<string> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/gamelist/games`,
      this.config.gamelistTimeoutMs,
    );
    if (!response.ok) throw new UpstreamError(response.status, `Gamelist upstream returned ${response.status}.`);
    return await response.text();
  }

  async fetchLua(appid: string): Promise<LuaResult> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/direct-lua?appid=${encodeURIComponent(appid)}`,
      this.config.upstreamTimeoutMs,
    );
    if (!response.ok) {
      throw new UpstreamError(response.status, `Lua upstream returned ${response.status} for AppID ${appid}.`);
    }
    const contentType = response.headers.get("content-type") ?? "text/plain; charset=utf-8";
    return { appid, body: await response.text(), contentType };
  }

  // The full fix catalog as text. Grouped by game, each with one or more fix variants.
  async fetchGameFixes(): Promise<string> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/game-fixes`,
      this.config.gamelistTimeoutMs,
    );
    if (!response.ok) throw new UpstreamError(response.status, `Game-fixes upstream returned ${response.status}.`);
    return await response.text();
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
      stream: Readable.fromWeb(response.body as Parameters<typeof Readable.fromWeb>[0]),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/zip",
    };
  }

  // Opens a streaming download of one fix archive by its opaque id. The caller pipes it straight
  // to the client so a large archive is never buffered in the proxy.
  async downloadFix(id: string): Promise<RawStream> {
    const response = await this.fetchWithTimeout(
      `${this.config.depotboxBase}/api/game-fixes/download?id=${encodeURIComponent(id)}`,
      this.config.upstreamTimeoutMs,
    );
    if (!response.ok || !response.body) {
      throw new UpstreamError(
        response.ok ? 502 : response.status,
        `Fix download upstream returned ${response.status} for id ${id}.`,
      );
    }
    const lengthHeader = response.headers.get("content-length");
    return {
      stream: Readable.fromWeb(response.body as Parameters<typeof Readable.fromWeb>[0]),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/octet-stream",
    };
  }
}
