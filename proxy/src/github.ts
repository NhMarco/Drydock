// Server-side GitHub client for the private MFB repo. The GitHub token lives ONLY here, so
// the app no longer needs it embedded — clients pull the OST service payload and the per-app
// fixes from the proxy instead of directly from GitHub.
//
// Everything goes through the GitHub **Contents API** (`/repos/{o}/{r}/contents/{path}?ref=…`),
// matching the Rust client (`crates/drydock-core/src/mfb.rs`): directory listings with the
// `application/vnd.github+json` media type, file downloads with `application/vnd.github.raw`.
// This deliberately avoids the Git Data API (`/git/trees`), which GitHub blocks for
// fine-grained tokens, and `raw.githubusercontent.com`, whose separate rate limits a single
// shared token quickly exhausts.

import { Readable } from "node:stream";
import type { Config } from "./config.js";

export class GitHubError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "GitHubError";
  }
}

// One entry from a Contents API directory listing.
export interface ContentEntry {
  name: string;
  path: string;
  sha: string;
  size?: number;
  type: string; // "file" | "dir" | ...
}

const ACCEPT_JSON = "application/vnd.github+json";
const ACCEPT_RAW = "application/vnd.github.raw";
const MAX_LISTING_BYTES = 32 * 1024 * 1024;

export interface RawStream {
  stream: Readable;
  contentLength: number | null;
  contentType: string;
}

export class GitHubClient {
  constructor(private readonly config: Config) {}

  private headers(accept: string): Record<string, string> {
    return {
      authorization: `Bearer ${this.config.githubToken}`,
      "user-agent": "drydock-proxy/1.0",
      accept,
      "x-github-api-version": "2022-11-28",
    };
  }

  private contentsUrl(path: string): string {
    const encoded = path.split("/").map(encodeURIComponent).join("/");
    const { githubOwner, githubRepo, githubBranch } = this.config;
    return `https://api.github.com/repos/${githubOwner}/${githubRepo}/contents/${encoded}?ref=${encodeURIComponent(githubBranch)}`;
  }

  private async send(url: string, accept: string): Promise<Response> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.config.upstreamTimeoutMs);
    try {
      return await fetch(url, { headers: this.headers(accept), signal: controller.signal });
    } catch (error) {
      if (error instanceof Error && error.name === "AbortError") {
        throw new GitHubError(504, "GitHub request timed out.");
      }
      throw new GitHubError(502, "GitHub request failed.");
    } finally {
      clearTimeout(timer);
    }
  }

  // Lists a repository directory. A missing directory (404) is treated as empty, so an absent
  // `Files/fix` simply means "no fixes" rather than an error (matches the Rust client).
  async listDirectory(path: string): Promise<ContentEntry[]> {
    const response = await this.send(this.contentsUrl(path), ACCEPT_JSON);
    if (response.status === 404) return [];
    if (!response.ok) throw new GitHubError(response.status, `GitHub listing returned ${response.status} for ${path}.`);
    const length = Number(response.headers.get("content-length") ?? "0");
    if (length > MAX_LISTING_BYTES) throw new GitHubError(502, "GitHub listing was too large.");
    const data = (await response.json()) as unknown;
    if (!Array.isArray(data)) throw new GitHubError(502, `GitHub listing for ${path} was not a directory.`);
    return data as ContentEntry[];
  }

  async fetchRawText(path: string): Promise<string> {
    const response = await this.send(this.contentsUrl(path), ACCEPT_RAW);
    if (!response.ok) throw new GitHubError(response.status, `GitHub raw returned ${response.status} for ${path}.`);
    return await response.text();
  }

  // Opens a streaming download for a repository file. The caller pipes it straight to the
  // client so a multi-hundred-MB fix zip part never has to be buffered in the proxy.
  async openRawStream(path: string): Promise<RawStream> {
    const response = await this.send(this.contentsUrl(path), ACCEPT_RAW);
    if (!response.ok || !response.body) {
      throw new GitHubError(response.ok ? 502 : response.status, `GitHub raw returned ${response.status} for ${path}.`);
    }
    const lengthHeader = response.headers.get("content-length");
    return {
      stream: Readable.fromWeb(response.body as Parameters<typeof Readable.fromWeb>[0]),
      contentLength: lengthHeader ? Number(lengthHeader) : null,
      contentType: response.headers.get("content-type") ?? "application/octet-stream",
    };
  }
}
