import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, rm, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough, Writable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createServer, type Server } from "node:http";
import { createHmac } from "node:crypto";
import Fastify, { type FastifyBaseLogger } from "fastify";
import { FileCache } from "./fileCache.js";
import { MergedGamelistSource } from "./merged.js";
import { GamelistCache } from "./gamelistCache.js";
import { SteamToolsClient, UpstreamError } from "./upstream.js";
import { DepotBoxClient } from "./depotbox.js";
import { GitHubClient } from "./github.js";
import { loadConfig, type Config } from "./config.js";
import { createAuthHook } from "./auth.js";
import { buildSigningString, NonceStore, verifyHmac } from "./hmac.js";

const log = { info() {}, warn() {}, error() {} } as unknown as FastifyBaseLogger;

test("HMAC matches the Rust known-answer vector", () => {
  const message = buildSigningString("GET", "/v1/lua/730", "1700000000", "abc123def4567890");
  assert.equal(createHmac("sha256", "test-secret").update(message).digest("base64"), "l+/pk0jM9UXgbgGYkAfnE+IS3yJ51BvqxMv8MMoY5cs=");
});

test("HMAC enforces timestamps, query binding and rotated secrets", () => {
  const timestamp=String(Math.floor(Date.now()/1000));
  const nonce="rotation-fixture-0123";
  const signature=createHmac("sha256","previous").update(buildSigningString("GET","/v1/lua/1?x=1",timestamp,nonce)).digest("base64");
  const request={method:"GET",url:"/v1/lua/1?x=1",headers:{"x-drydock-timestamp":timestamp,"x-drydock-nonce":nonce,"x-drydock-signature":signature}};
  const options={secrets:["current","previous"],windowSeconds:60,nonceStore:new NonceStore(60)};
  assert.equal(verifyHmac({...request,url:"/v1/lua/1?x=2"} as never,options).ok,false);
  assert.equal(verifyHmac({...request,headers:{...request.headers,"x-drydock-timestamp":"0"}} as never,options).ok,false);
  assert.equal(verifyHmac(request as never,options).ok,true);
});

test("sweep preserves a currently streamed cache file", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-cache-"));
  try {
    const cache = new FileCache(dir, 1000, log);
    const stream = new PassThrough();
    const writing = cache.put("active", stream, "application/zip");
    stream.write("begin");
    for (let n = 0; n < 100; n++) {
      if ((await readdir(join(dir, "filecache")).catch(() => [])).some(name => name.endsWith(".tmp"))) break;
      await new Promise(resolve => setTimeout(resolve, 5));
    }
    await cache.sweep();
    stream.end("end");
    assert.equal((await writing).contentLength, 8);
    assert.ok(await cache.get("active"));
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test("invalid provider body cannot replace a good snapshot", async () => {
  const dir = await mkdtemp(join(tmpdir(), "drydock-catalog-"));
  try {
    let body = JSON.stringify({games:[{appid:1,name:"Game"}]});
    const source = new MergedGamelistSource([{name:"fixture",source:{fetchGamelist:async()=>body}}],log);
    const cache = new GamelistCache({dataDir:dir,filterNsfw:false} as Config,source,log);
    await cache.refresh();
    const original = cache.current;
    for (const invalid of ["<html>Maintenance</html>", '{"error":"offline"}', '{"games":[]}', '{"games":[{"appid":2}]}']) {
      body = invalid;
      await assert.rejects(cache.refresh());
      assert.equal(cache.current, original);
    }
  } finally { await rm(dir, {recursive:true,force:true}); }
});

// A local upstream that sends its headers and the start of a body, then stalls.
async function stallingServer(firstBytes: string): Promise<{ server: Server; base: string }> {
  const server = createServer((_req,res) => { res.writeHead(200); res.write(firstBytes); });
  await new Promise<void>(resolve => server.listen(0,"127.0.0.1",resolve));
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  return { server, base: `http://127.0.0.1:${address.port}` };
}

async function stop(server: Server): Promise<void> {
  server.closeAllConnections();
  await new Promise<void>(resolve=>server.close(()=>resolve()));
}

const timedOut = (error: unknown) => error instanceof UpstreamError && error.status === 504;

// Without the deadline undici's own body timeout would still end the request, minutes later.
test("deadline aborts body after headers have arrived", { timeout: 5000 }, async () => {
  const { server, base } = await stallingServer('{"games":[');
  try {
    const client = new SteamToolsClient({upstreamBase:base,upstreamApiKey:"test",gamelistTimeoutMs:150} as Config);
    const started = Date.now();
    await assert.rejects(client.fetchGamelist(), timedOut);
    assert.ok(Date.now() - started < 2000, "the deadline ended the request");
  } finally { await stop(server); }
});

test("a depot package that stalls mid-body fails as a timeout", { timeout: 5000 }, async () => {
  const { server, base } = await stallingServer("PK");
  try {
    const client = new DepotBoxClient({depotboxBase:base,depotboxApiKey:"test",depotPackageTimeoutMs:150} as Config);
    const raw = await client.downloadDepotPackage("1");
    const sink = new Writable({ write(_chunk, _encoding, done) { done(); } });
    await assert.rejects(pipeline(raw.stream, sink), timedOut);
  } finally { await stop(server); }
});

test("invalid REQUIRE_AUTH cannot silently disable protection", () => {
  const old = process.env.REQUIRE_AUTH;
  try { process.env.REQUIRE_AUTH="ture"; assert.throws(loadConfig, /Invalid boolean for REQUIRE_AUTH/); }
  finally { if(old===undefined) delete process.env.REQUIRE_AUTH; else process.env.REQUIRE_AUTH=old; }
});

test("a streamed GitHub download outlives the request deadline", async () => {
  const previous = globalThis.fetch;
  globalThis.fetch = async (_url, init) => {
    const signal = init?.signal;
    let sent = 0;
    // Three chunks 60 ms apart: longer in total than the 100 ms deadline, as a large fix part read at
    // the client's pace would be.
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        await new Promise((resolve) => setTimeout(resolve, 60));
        if (signal?.aborted) return controller.error(signal.reason);
        if (sent === 3) return controller.close();
        sent += 1;
        controller.enqueue(new Uint8Array([sent]));
      },
    });
    return new Response(body);
  };
  try {
    const client = new GitHubClient({githubToken:"",githubOwner:"fixture",githubRepo:"fixture",githubBranch:"main",upstreamTimeoutMs:100} as Config);
    const raw = await client.openRawStream("fix/1.zip.001");
    const chunks: Buffer[] = [];
    for await (const chunk of raw.stream) chunks.push(chunk as Buffer);
    assert.equal(Buffer.concat(chunks).length, 3);
  } finally { globalThis.fetch=previous; }
});

test("GitHub public access omits empty credentials", async () => {
  const previous = globalThis.fetch;
  let authorization: string | null = null;
  globalThis.fetch = async (_url, init) => {
    authorization = new Headers(init?.headers).get("authorization");
    return new Response("[]");
  };
  try {
    await new GitHubClient({githubToken:"",githubOwner:"fixture",githubRepo:"fixture",githubBranch:"main",upstreamTimeoutMs:1000} as Config).listDirectory("files");
    assert.equal(authorization,null);
  } finally { globalThis.fetch=previous; }
});

test("auth rejects missing/bad signatures and replay before reaching handler", async () => {
  const app = Fastify();
  const secret="fixture-secret";
  let called=0;
  app.get("/protected",{preHandler:createAuthHook({requireAuth:true,hmacSecrets:[secret],hmacWindowSeconds:60} as Config)},async()=>{called++;return {ok:true};});
  try {
    assert.equal((await app.inject({url:"/protected"})).statusCode,401);
    const timestamp=String(Math.floor(Date.now()/1000)), nonce="0123456789abcdef";
    const canonical=`GET\n/protected\n${timestamp}\n${nonce}`;
    const headers={"x-drydock-timestamp":timestamp,"x-drydock-nonce":nonce,"x-drydock-signature":createHmac("sha256",secret).update(canonical).digest("base64")};
    assert.equal((await app.inject({url:"/protected",headers:{...headers,"x-drydock-signature":"bad"}})).statusCode,401);
    assert.equal((await app.inject({url:"/protected",headers})).statusCode,200);
    assert.equal((await app.inject({url:"/protected",headers})).statusCode,401);
    assert.equal(called,1);
  } finally { await app.close(); }
});
