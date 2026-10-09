// Which games a fix is offered for: only those that run from Steam alone. A game that starts through
// a third-party launcher (EA app, Ubisoft Connect, Rockstar Games Launcher) needs more than a fix
// carries, so `FIX_BLOCKED_LAUNCHERS` leaves those out. What a game needs is read from its Steam store
// page — the publisher and the "requires a third-party account" notice — once a week per game, kept
// on disk so a restart does not ask Steam again.
//
// A game is offered only once it is known to run from Steam alone: until its store page has been
// read it is held back, so a launcher game can never slip through while Steam is slow or down.

import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import type { FastifyBaseLogger } from "fastify";
import { fetchWithDeadline, readBody, type Deadline } from "./http.js";

export const LAUNCHERS = ["ea", "ubisoft", "rockstar"] as const;
export type Launcher = (typeof LAUNCHERS)[number];

/** The part of Steam's `appdetails` this looks at. */
export interface StoreFacts {
  publishers?: unknown;
  ext_user_account_notice?: unknown;
}

// The account notice names the launcher ("EA Account", "Uplay", "Ubisoft Connect launcher",
// "Rockstar Games"); the publisher catches a game whose page leaves the notice out (Dead Space).
const RULES: { launcher: Launcher; notice: RegExp; publisher: RegExp }[] = [
  { launcher: "ea", notice: /\bEA\b|Electronic Arts|\bOrigin\b/, publisher: /^Electronic Arts\b/i },
  { launcher: "ubisoft", notice: /Ubisoft|Uplay/i, publisher: /^Ubisoft\b/i },
  { launcher: "rockstar", notice: /Rockstar/i, publisher: /^Rockstar Games\b/i },
];

export function launcherOf(facts: StoreFacts): Launcher | null {
  const notice = typeof facts.ext_user_account_notice === "string" ? facts.ext_user_account_notice : "";
  const publishers = Array.isArray(facts.publishers)
    ? facts.publishers.filter((value): value is string => typeof value === "string").map((value) => value.trim())
    : [];
  for (const rule of RULES) {
    if (rule.notice.test(notice) || publishers.some((publisher) => rule.publisher.test(publisher))) {
      return rule.launcher;
    }
  }
  return null;
}

/** `FIX_BLOCKED_LAUNCHERS`: unset means all of them, "none" (or empty) means no filter. */
export function parseBlockedLaunchers(raw: string | undefined): Launcher[] {
  if (raw === undefined) return [...LAUNCHERS];
  const names = raw
    .split(",")
    .map((value) => value.trim().toLowerCase())
    .filter((value) => value.length > 0 && value !== "none");
  for (const name of names) {
    if (!(LAUNCHERS as readonly string[]).includes(name)) {
      throw new Error(`Invalid FIX_BLOCKED_LAUNCHERS entry: ${name} (allowed: ${LAUNCHERS.join(", ")}, none)`);
    }
  }
  return [...new Set(names)] as Launcher[];
}

interface Verdict {
  launcher: Launcher | null;
  checkedAt: number;
}

export interface LauncherIndexOptions {
  blocked: readonly Launcher[];
  /** Where the verdicts are kept between restarts. */
  file: string;
  /** Reads a game's store facts; `null` when Steam has no page for it. Throws when Steam can't be asked. */
  fetchFacts: (appid: number) => Promise<StoreFacts | null>;
  log: FastifyBaseLogger;
  maxAgeMs?: number;
  retryAfterMs?: number;
  spacingMs?: number;
}

export class LauncherIndex {
  private readonly blocked: ReadonlySet<Launcher>;
  private readonly verdicts = new Map<number, Verdict>();
  private readonly failedAt = new Map<number, number>();
  private readonly maxAgeMs: number;
  private readonly retryAfterMs: number;
  private readonly spacingMs: number;
  private readonly loaded: Promise<void>;
  private running: Promise<void> | null = null;

  constructor(private readonly options: LauncherIndexOptions) {
    this.blocked = new Set(options.blocked);
    this.maxAgeMs = options.maxAgeMs ?? 7 * 24 * 60 * 60 * 1000;
    this.retryAfterMs = options.retryAfterMs ?? 10 * 60 * 1000;
    this.spacingMs = options.spacingMs ?? 1500;
    this.loaded = this.blocked.size === 0 ? Promise.resolve() : this.load();
  }

  /** Whether a fix for `appid` may be offered. */
  allows(appid: number): boolean {
    if (this.blocked.size === 0) return true;
    const verdict = this.verdicts.get(appid);
    if (!verdict) return false;
    return verdict.launcher === null || !this.blocked.has(verdict.launcher);
  }

  /** Reads the store pages of the games not known yet (or too long ago), one at a time, in the background. */
  check(appids: readonly number[]): Promise<void> {
    if (this.blocked.size === 0) return Promise.resolve();
    if (this.running) return this.running;
    this.running = this.loaded
      .then(() => this.pass(appids))
      .catch((error) => this.options.log.warn({ err: error }, "Launcher check failed."))
      .finally(() => {
        this.running = null;
      });
    return this.running;
  }

  private due(appid: number, now: number): boolean {
    const verdict = this.verdicts.get(appid);
    if (verdict && now - verdict.checkedAt < this.maxAgeMs) return false;
    const failed = this.failedAt.get(appid);
    return failed === undefined || now - failed >= this.retryAfterMs;
  }

  private async pass(appids: readonly number[]): Promise<void> {
    const due = [...new Set(appids)].filter((appid) => this.due(appid, Date.now()));
    if (due.length === 0) return;
    let changed = false;
    for (const [index, appid] of due.entries()) {
      if (index > 0) await new Promise((resolve) => setTimeout(resolve, this.spacingMs));
      try {
        const facts = await this.options.fetchFacts(appid);
        this.verdicts.set(appid, { launcher: facts ? launcherOf(facts) : null, checkedAt: Date.now() });
        this.failedAt.delete(appid);
        changed = true;
      } catch (error) {
        this.failedAt.set(appid, Date.now());
        this.options.log.warn({ err: error, appid }, "Could not read a fix game's store page.");
      }
    }
    if (changed) await this.save();
    const held = due.filter((appid) => !this.allows(appid));
    this.options.log.info({ checked: due.length, held: held.length }, "Fix launcher check done.");
  }

  private async load(): Promise<void> {
    try {
      const stored = JSON.parse(await readFile(this.options.file, "utf8")) as Record<string, Verdict>;
      for (const [appid, verdict] of Object.entries(stored)) {
        const id = Number.parseInt(appid, 10);
        const launcher = verdict?.launcher ?? null;
        if (!Number.isInteger(id) || typeof verdict?.checkedAt !== "number") continue;
        if (launcher !== null && !(LAUNCHERS as readonly string[]).includes(launcher)) continue;
        this.verdicts.set(id, { launcher, checkedAt: verdict.checkedAt });
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
        this.options.log.warn({ err: error }, "Stored launcher verdicts unreadable; asking Steam again.");
      }
    }
  }

  private async save(): Promise<void> {
    const body = JSON.stringify(Object.fromEntries(this.verdicts));
    const temporary = `${this.options.file}.tmp`;
    await mkdir(dirname(this.options.file), { recursive: true });
    await writeFile(temporary, body);
    await rename(temporary, this.options.file);
  }
}

const STORE_DEADLINE: Deadline = {
  label: "Steam store",
  error: (status, message) => Object.assign(new Error(message), { status }),
};

/** Reads one game's facts from Steam's public `appdetails`. */
export function steamStoreFacts(storeBase: string): (appid: number) => Promise<StoreFacts | null> {
  return async (appid) => {
    const url = `${storeBase}/api/appdetails?appids=${appid}&l=english&cc=us`;
    const response = await fetchWithDeadline(url, {}, 15_000, STORE_DEADLINE);
    if (!response.ok) throw STORE_DEADLINE.error(response.status, `Steam store answered ${response.status}.`);
    const body = (await readBody(response.json(), STORE_DEADLINE)) as Record<string, { success?: boolean; data?: StoreFacts }>;
    const entry = body?.[String(appid)];
    if (!entry || typeof entry !== "object") throw new Error("Steam store answered without the game.");
    return entry.success && entry.data ? entry.data : null;
  };
}
