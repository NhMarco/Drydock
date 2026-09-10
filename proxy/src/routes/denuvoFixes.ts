// Serves the per-app "Denuvo fixes" (build-locked Lua + game-folder zip parts) from GitHub.
//
//   GET /v1/denuvo-fixes            -> { fixes: [{ appid, lua: {name,sha,size}, zip_parts: [...] }] }
//   GET /v1/denuvo-fixes/file/:name -> raw bytes of one fix file (streamed from GitHub)
//
// A file is only servable if it appears in the current manifest (whitelist), so the route can
// never pull an arbitrary repo path. The client verifies each file's git-blob sha, so the proxy
// streams without re-hashing.

import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import { GitHubError, type GitHubClient } from "../github.js";
import { classifyFixFile, type DenuvoFixesCache, type FixFileRef } from "../denuvoFixesCache.js";

function publicRef(ref: FixFileRef): { name: string; sha: string; size?: number } {
  return { name: ref.name, sha: ref.sha, ...(ref.size !== undefined ? { size: ref.size } : {}) };
}

// Accepts only `{appid}.lua`, `{appid}.zip`, `{appid}.zip.NNN` shapes before the whitelist check.
const NAME_PATTERN = /^[0-9]{1,10}\.(lua|zip)(\.[0-9]{1,10})?$/i;

export function registerDenuvoFixesRoutes(
  app: FastifyInstance,
  github: GitHubClient,
  cache: DenuvoFixesCache,
  authHook: preHandlerHookHandler,
): void {
  app.get("/v1/denuvo-fixes", { preHandler: authHook }, async (req, reply) => {
    try {
      const fixes = await cache.getFixes();
      const body = fixes.map((fix) => ({
        appid: fix.appid,
        lua: publicRef(fix.lua),
        zip_parts: fix.zip_parts.map(publicRef),
      }));
      return reply.code(200).header("Cache-Control", "no-store").send({ fixes: body });
    } catch (error) {
      return sendError(reply, req.log, error, "denuvo-fixes manifest");
    }
  });

  app.get<{ Params: { name: string } }>(
    "/v1/denuvo-fixes/file/:name",
    { preHandler: authHook },
    async (req, reply) => {
      const { name } = req.params;
      if (!NAME_PATTERN.test(name) || !classifyFixFile(name)) {
        return reply.code(400).send({ error: "invalid_filename" });
      }
      try {
        const file = await cache.resolveFile(name);
        if (!file) return reply.code(404).send({ error: "not_found" });
        const raw = await github.openRawStream(file.path);
        reply
          .header("Content-Type", "application/octet-stream")
          .header("Cache-Control", "no-store")
          .header("X-Content-Git-Sha", file.sha)
          .header("Content-Disposition", `attachment; filename="${file.name}"`);
        if (raw.contentLength !== null) reply.header("Content-Length", String(raw.contentLength));
        return reply.send(raw.stream);
      } catch (error) {
        return sendError(reply, req.log, error, `denuvo fix file ${name}`);
      }
    },
  );
}

function sendError(reply: FastifyReply, log: FastifyBaseLogger, error: unknown, context: string): FastifyReply {
  if (error instanceof GitHubError) {
    const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
    log.warn({ context, githubStatus: error.status }, "GitHub error while serving denuvo fix payload.");
    return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
  }
  log.error({ err: error, context }, "Unexpected denuvo fix route error.");
  return reply.code(502).send({ error: "upstream_error" });
}
