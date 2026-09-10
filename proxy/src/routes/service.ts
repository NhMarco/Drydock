// Serves the Steam Service payload (OST DLLs) through the proxy so clients no longer pull
// from GitHub directly and the app needs no embedded GitHub token.
//
//   GET /v1/service/manifest      -> { version, files: [{ name, sha, size }] }
//   GET /v1/service/file/:name    -> the raw file bytes (streamed from GitHub)
//
// The manifest is briefly cached; the file bytes are streamed fresh from GitHub on each
// request. The client verifies each file against the manifest's git-blob sha, so the proxy
// does not need to buffer the whole file to re-hash it.

import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import { GitHubError, type GitHubClient } from "../github.js";
import type { ServiceCache } from "../serviceCache.js";

const NAME_PATTERN = /^[A-Za-z0-9._-]{1,128}$/;

export function registerServiceRoutes(
  app: FastifyInstance,
  github: GitHubClient,
  cache: ServiceCache,
  authHook: preHandlerHookHandler,
): void {
  app.get("/v1/service/manifest", { preHandler: authHook }, async (req, reply) => {
    try {
      const manifest = await cache.getManifest();
      return reply
        .code(200)
        .header("Cache-Control", "no-store")
        .send({
          version: manifest.version,
          files: manifest.files.map((f) => ({ name: f.name, sha: f.sha, ...(f.size !== undefined ? { size: f.size } : {}) })),
        });
    } catch (error) {
      return sendError(reply, req.log, error, "service manifest");
    }
  });

  app.get<{ Params: { name: string } }>(
    "/v1/service/file/:name",
    { preHandler: authHook },
    async (req, reply) => {
      const { name } = req.params;
      if (!NAME_PATTERN.test(name)) return reply.code(400).send({ error: "invalid_filename" });
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
        return sendError(reply, req.log, error, `service file ${name}`);
      }
    },
  );
}

function sendError(reply: FastifyReply, log: FastifyBaseLogger, error: unknown, context: string): FastifyReply {
  if (error instanceof GitHubError) {
    const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
    log.warn({ context, githubStatus: error.status }, "GitHub error while serving service payload.");
    return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
  }
  log.error({ err: error, context }, "Unexpected service route error.");
  return reply.code(502).send({ error: "upstream_error" });
}
