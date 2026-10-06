// Run with `npm test` in proxy/.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createHmac, randomBytes } from "node:crypto";
import Fastify, { type FastifyInstance } from "fastify";
import type { Config } from "./config.js";
import { attachSession, clientKey, createAuthHook } from "./auth.js";
import { registerDiscordAuthRoutes, type DiscordApi } from "./discordAuth.js";
import { buildSigningString } from "./hmac.js";
import { issueSession, verifySession } from "./session.js";

const HMAC = "test-hmac-secret";
const SESSION = "s".repeat(40);

function config(overrides: Partial<Config> = {}): Config {
  return {
    requireAuth: true,
    hmacSecrets: [HMAC],
    hmacWindowSeconds: 60,
    discordClientId: "123",
    discordClientSecret: "client-secret",
    discordRedirectUri: "https://proxy.example/v1/auth/discord/callback",
    discordApiBase: "https://discord.example/api",
    sessionSecrets: [SESSION],
    sessionTtlDays: 30,
    requireLogin: true,
    bannedDiscordIds: new Set(["666"]),
    ...overrides,
  } as Config;
}

/** Headers for a request signed the way the app signs it. */
function signed(path: string): Record<string, string> {
  const timestamp = String(Math.floor(Date.now() / 1000));
  const nonce = randomBytes(16).toString("hex");
  const signature = createHmac("sha256", HMAC)
    .update(buildSigningString("GET", path, timestamp, nonce), "utf8")
    .digest("base64");
  return { "x-drydock-timestamp": timestamp, "x-drydock-nonce": nonce, "x-drydock-signature": signature };
}

const discord: DiscordApi = {
  async userForCode(code) {
    if (code === "banned") return { id: "666", username: "troll" };
    if (code !== "good") throw new Error("bad code");
    return { id: "4242", username: "marco", global_name: "Marco" };
  },
};

function app(cfg: Config): FastifyInstance {
  const server = Fastify();
  attachSession(server, cfg);
  registerDiscordAuthRoutes(server, cfg, discord, createAuthHook(cfg, { allowWithoutLogin: true }));
  server.get("/v1/protected", { preHandler: createAuthHook(cfg) }, async (req) => ({ who: clientKey(req) }));
  return server;
}

// --- Session tokens ---------------------------------------------------------------------------

test("a session token names its user until it expires, and only with a current key", () => {
  const now = 1_800_000_000;
  const token = issueSession({ id: "4242", name: "Marco" }, [SESSION], 60, now);
  assert.deepEqual(verifySession(token, [SESSION], new Set(), now), { id: "4242", name: "Marco", expiresAt: now + 60 });
  assert.equal(verifySession(token, [SESSION], new Set(), now + 60), null, "expired");
  assert.equal(verifySession(token, ["t".repeat(40)], new Set(), now), null, "signed with another key");
  // A rotated key: the new one signs, the old one still verifies until it is dropped.
  assert.ok(verifySession(token, ["t".repeat(40), SESSION], new Set(), now));
  assert.equal(verifySession(token, [SESSION], new Set(["4242"]), now), null, "banned");
});

test("a session token cannot be edited", () => {
  const token = issueSession({ id: "4242", name: "Marco" }, [SESSION], 60);
  const [prefix, , signature] = token.split(".");
  const forged = Buffer.from(JSON.stringify({ uid: "1", name: "x", iat: 0, exp: 9_999_999_999 })).toString("base64url");
  assert.equal(verifySession(`${prefix}.${forged}.${signature}`, [SESSION], new Set()), null);
  assert.equal(verifySession("dl1.garbage", [SESSION], new Set()), null);
  assert.equal(verifySession("x".repeat(5000), [SESSION], new Set()), null);
});

// --- The client address behind the reverse proxy ----------------------------------------------

test("a made-up X-Forwarded-For does not change who a request is counted as", async () => {
  const server = Fastify({ trustProxy: "loopback,linklocal,uniquelocal" });
  server.get("/ip", async (req) => ({ ip: req.ip }));
  // Through Nginx Proxy Manager (a Docker-network peer): the address it appended counts, not the
  // one the client wrote in front of it.
  const proxied = await server.inject({
    url: "/ip",
    remoteAddress: "172.18.0.2",
    headers: { "x-forwarded-for": "1.2.3.4, 203.0.113.9" },
  });
  assert.equal(proxied.json().ip, "203.0.113.9");
  // Straight from the internet, the header is not believed at all.
  const direct = await server.inject({
    url: "/ip",
    remoteAddress: "198.51.100.7",
    headers: { "x-forwarded-for": "1.2.3.4" },
  });
  assert.equal(direct.json().ip, "198.51.100.7");
  await server.close();
});

// --- The Discord login --------------------------------------------------------------------------

test("the whole login: Discord, back to the app's own listener, one-time ticket, then access", async () => {
  const cfg = config();
  const server = app(cfg);
  try {
    const state = randomBytes(32).toString("base64url");

    // Without a session, the HMAC alone no longer gets anywhere.
    const before = await server.inject({ url: "/v1/protected", headers: signed("/v1/protected") });
    assert.equal(before.statusCode, 401);
    assert.equal(before.json().error, "login_required");

    const login = await server.inject({ url: `/v1/auth/discord/login?port=50123&state=${state}` });
    assert.equal(login.statusCode, 302);
    const authorize = new URL(login.headers.location as string);
    assert.equal(authorize.origin + authorize.pathname, "https://discord.example/api/oauth2/authorize");
    assert.equal(authorize.searchParams.get("state"), state);
    assert.equal(authorize.searchParams.get("scope"), "identify");
    assert.equal(authorize.searchParams.get("client_id"), "123");
    assert.ok(!(login.headers.location as string).includes("client-secret"), "the secret never leaves");

    const back = await server.inject({ url: `/v1/auth/discord/callback?code=good&state=${state}` });
    assert.equal(back.statusCode, 302);
    const toApp = new URL(back.headers.location as string);
    assert.equal(toApp.origin, "http://127.0.0.1:50123");
    assert.equal(toApp.searchParams.get("state"), state);
    const ticket = toApp.searchParams.get("ticket") ?? "";
    assert.ok(ticket.length >= 32);
    assert.ok(!toApp.search.includes("dl1."), "the session token itself never goes through the browser");

    // The same login cannot be completed twice.
    assert.equal((await server.inject({ url: `/v1/auth/discord/callback?code=good&state=${state}` })).statusCode, 400);

    // The ticket needs the state the app made up, and is good once.
    const wrongPath = `/v1/auth/discord/redeem?ticket=${ticket}&state=${"x".repeat(43)}`;
    assert.equal((await server.inject({ url: wrongPath, headers: signed(wrongPath) })).statusCode, 404);
    const redeemPath = `/v1/auth/discord/redeem?ticket=${ticket}&state=${state}`;
    // That wrong guess spent the ticket: a ticket is taken off the table whoever presents it.
    assert.equal((await server.inject({ url: redeemPath, headers: signed(redeemPath) })).statusCode, 404);
  } finally {
    await server.close();
  }
});

test("a redeemed ticket gives the session that opens the protected routes", async () => {
  const cfg = config();
  const server = app(cfg);
  try {
    const state = randomBytes(32).toString("base64url");
    await server.inject({ url: `/v1/auth/discord/login?port=50124&state=${state}` });
    const back = await server.inject({ url: `/v1/auth/discord/callback?code=good&state=${state}` });
    const ticket = new URL(back.headers.location as string).searchParams.get("ticket");
    const redeemPath = `/v1/auth/discord/redeem?ticket=${ticket}&state=${state}`;
    const redeemed = await server.inject({ url: redeemPath, headers: signed(redeemPath) });
    assert.equal(redeemed.statusCode, 200);
    const { token, user } = redeemed.json();
    assert.deepEqual(user, { id: "4242", name: "Marco" });
    assert.equal((await server.inject({ url: redeemPath, headers: signed(redeemPath) })).statusCode, 404, "once");

    const allowed = await server.inject({
      url: "/v1/protected",
      headers: { ...signed("/v1/protected"), authorization: `Bearer ${token}` },
    });
    assert.equal(allowed.statusCode, 200);
    assert.equal(allowed.json().who, "user:4242", "limits count the account, not the address");

    // The token alone, without the request signature, is not enough either.
    const unsigned = await server.inject({ url: "/v1/protected", headers: { authorization: `Bearer ${token}` } });
    assert.equal(unsigned.statusCode, 401);

    const session = await server.inject({
      url: "/v1/auth/session",
      headers: { ...signed("/v1/auth/session"), authorization: `Bearer ${token}` },
    });
    assert.equal(session.json().user.name, "Marco");
    assert.deepEqual(session.json().login, { available: true, required: true });
  } finally {
    await server.close();
  }
});

test("a banned account, a refused consent and a broken link all end without a session", async () => {
  const server = app(config());
  try {
    for (const [code, error] of [
      ["banned", "banned"],
      ["unknown", "discord"],
    ] as const) {
      const state = randomBytes(32).toString("base64url");
      await server.inject({ url: `/v1/auth/discord/login?port=50125&state=${state}` });
      const back = await server.inject({ url: `/v1/auth/discord/callback?code=${code}&state=${state}` });
      const url = new URL(back.headers.location as string);
      assert.equal(url.searchParams.get("error"), error);
      assert.equal(url.searchParams.get("ticket"), null);
    }
    const state = randomBytes(32).toString("base64url");
    await server.inject({ url: `/v1/auth/discord/login?port=50125&state=${state}` });
    const denied = await server.inject({ url: `/v1/auth/discord/callback?error=access_denied&state=${state}` });
    assert.equal(new URL(denied.headers.location as string).searchParams.get("error"), "denied");

    assert.equal((await server.inject({ url: "/v1/auth/discord/login?port=80&state=" + state })).statusCode, 400);
    assert.equal((await server.inject({ url: "/v1/auth/discord/login?port=50125&state=short" })).statusCode, 400);
  } finally {
    await server.close();
  }
});

test("while login is not required, the HMAC alone still works, and login reports itself off", async () => {
  const server = app(config({ requireLogin: false, discordClientId: "" }));
  try {
    const open = await server.inject({ url: "/v1/protected", headers: signed("/v1/protected") });
    assert.equal(open.statusCode, 200);
    const session = await server.inject({ url: "/v1/auth/session", headers: signed("/v1/auth/session") });
    assert.deepEqual(session.json(), { login: { available: false, required: false }, user: null });
    assert.equal((await server.inject({ url: `/v1/auth/discord/login?port=50126&state=${"a".repeat(43)}` })).statusCode, 503);
  } finally {
    await server.close();
  }
});

// --- Limits -------------------------------------------------------------------------------------

test("one client cannot spend the whole day-limited allowance", async () => {
  const { mkdtemp, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { Readable } = await import("node:stream");
  const { FileCache } = await import("./fileCache.js");
  const { ProviderCooldown } = await import("./merged.js");
  const { registerDepotRoutes } = await import("./routes/depot.js");
  const { UpstreamError } = await import("./upstream.js");

  // A package as the size check reads it: local file headers naming a manifest.
  const header = Buffer.alloc(30);
  header.writeUInt32LE(0x04034b50, 0);
  header.writeUInt16LE("1_2.manifest".length, 26);
  const zip = Buffer.concat([header, Buffer.from("1_2.manifest"), Buffer.from("content")]);
  const raw = () => ({ stream: Readable.from([zip]), contentLength: zip.length, contentType: "application/zip" });

  const dir = await mkdtemp(join(tmpdir(), "drydock-limits-"));
  const server = Fastify({ trustProxy: "loopback,linklocal,uniquelocal" });
  const log = { info() {}, warn() {}, error() {} } as never;
  try {
    let hubcapCalls = 0;
    const upstreams = {
      ryu: { downloadDepotPackage: async () => { throw new UpstreamError(404, "not here"); } },
      hubcap: {
        downloadDepotPackage: async () => { hubcapCalls += 1; return raw(); },
        hasAllowance: async () => true,
      },
    } as never;
    registerDepotRoutes(server, upstreams, ["ryu", "hubcap"], new FileCache(dir, 60_000, log), async () => {}, new ProviderCooldown(), {
      rateMax: 100,
      rateWindowMs: 60_000,
      lastResortPerClientDaily: 1,
    });
    const from = (ip: string, appid: number) =>
      server.inject({ url: `/v1/depot/package/${appid}`, remoteAddress: "172.18.0.2", headers: { "x-forwarded-for": ip } });

    assert.equal((await from("203.0.113.1", 70)).statusCode, 200);
    // The same client's next day-limited fetch is refused…
    assert.notEqual((await from("203.0.113.1", 71)).statusCode, 200);
    // …while another client still gets its share, and a package already fetched costs nothing.
    assert.equal((await from("203.0.113.2", 71)).statusCode, 200);
    assert.equal((await from("203.0.113.1", 70)).statusCode, 200);
    assert.equal(hubcapCalls, 2);
  } finally {
    await server.close();
    await rm(dir, { recursive: true, force: true });
  }
});
