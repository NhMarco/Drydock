// Thin client for the real steamtools.app API. The private x-api-key lives only here,
// server-side, and is never exposed to proxy clients.

import { Readable } from "node:stream";
import type { Config } from "./config.js";
import type { RawStream } from "./depotbox.js";

export class UpstreamError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "UpstreamError";
  }
}

export interface LuaResult {
  appid: string;
  body: string;
  contentType: string;
}

export class SteamToolsClient {
  constructor(private readonly config: Config) {}

  private headers(): Record<string, string> {
    return {
      "x-api-key": this.config.upstreamApiKey,
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
        throw new UpstreamError(504, "Upstream request timed out.");
      }
      throw new UpstreamError(502, "Upstream request failed.");
    } finally {
      clearTimeout(timer);
    }
  }

  // Downloads the full gamelist as text (~35 MB). Called only by the background refresher.
  async fetchGamelist(): Promise<string> {
    const response = await this.fetchWithTimeout(
      `${this.config.upstreamBase}/api/gamelist`,
      this.config.gamelistTimeoutMs,
    );
    if (!response.ok) throw new UpstreamError(response.status, `Gamelist upstream returned ${response.status}.`);
    return await response.text();
  }

  async fetchLua(appid: string): Promise<LuaResult> {
    const response = await this.fetchWithTimeout(
      `${this.config.upstreamBase}/api/lua/${appid}`,
      this.config.upstreamTimeoutMs,
    );
    if (!response.ok) {
      throw new UpstreamError(response.status, `Lua upstream returned ${response.status} for AppID ${appid}.`);
    }
    const contentType = response.headers.get("content-type") ?? "text/plain; charset=utf-8";
    return { appid, body: await response.text(), contentType };
  }

  // Opens a streaming download of the per-app manifest package (a ZIP of all `.manifest` files plus
  // the `.lua`, from which depot keys can be read). The DepotBox depot package is preferred where
  // available; this is the fallback source for Drydock' native depot download.
  async downloadManifestZip(appid: string): Promise<RawStream> {
    const response = await this.fetchWithTimeout(
      `${this.config.upstreamBase}/api/manifest/${encodeURIComponent(appid)}`,
      this.config.depotPackageTimeoutMs,
    );
    if (!response.ok || !response.body) {
      throw new UpstreamError(
        response.ok ? 502 : response.status,
        `Manifest upstream returned ${response.status} for AppID ${appid}.`,
      );
    }
    const lengthHeader = response.headers.get("content-length");
    return {
      stream: Readable.fromWeb(response.body as Parameters<typeof Readable.fromWeb>[0]),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/zip",
    };
  }

  // Per-app metadata (name, app_type, technologies incl. Denuvo, tags, reviews). Used to
  // enrich the flat catalog on demand for the app the user opened.
  async fetchAppInfo(appid: string): Promise<{ body: string; contentType: string }> {
    const response = await this.fetchWithTimeout(
      `${this.config.upstreamBase}/api/app-info?appid=${appid}`,
      this.config.upstreamTimeoutMs,
    );
    if (!response.ok) {
      throw new UpstreamError(response.status, `App-info upstream returned ${response.status} for AppID ${appid}.`);
    }
    const contentType = response.headers.get("content-type") ?? "application/json; charset=utf-8";
    return { body: await response.text(), contentType };
  }
}
