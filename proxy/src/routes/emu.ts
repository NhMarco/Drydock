// Serves the emulator DLLs the cracker deploys into a game folder, so the client needs no embedded
// GitHub token and the binaries can be updated without shipping a new Drydock release.
//
//   GET /v1/emu/manifest    -> { files: [{ name, sha, size }] }
//   GET /v1/emu/file/:name  -> the raw bytes (streamed from GitHub)
//
// Mirrors the Steam Service routes: the listing is briefly cached, file bytes are streamed fresh,
// and the client verifies each download against the git-blob sha the manifest advertised — so the
// proxy never has to buffer a 18 MB DLL to re-hash it.

import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import { GitHubError, type GitHubClient } from "../github.js";
import type { EmuCache } from "../emuCache.js";

const NAME_PATTERN = /^[A-Za-z0-9._-]{1,128}$/;

export function registerEmuRoutes(
  app: FastifyInstance,
  github: GitHubClient,
  cache: EmuCache,
  authHook: preHandlerHookHandler,
): void {
  app.get("/v1/emu/manifest", { preHandler: authHook }, async (req, reply) => {
    try {
      const files = await cache.getFiles();
      return reply
        .code(200)
        .header("Cache-Control", "no-store")
        .send({
          files: files.map((file) => ({
            name: file.name,
            sha: file.sha,
            ...(file.size !== undefined ? { size: file.size } : {}),
          })),
        });
    } catch (error) {
      return sendError(reply, req.log, error, "emu manifest");
    }
  });

  app.get<{ Params: { name: string } }>("/v1/emu/file/:name", { preHandler: authHook }, async (req, reply) => {
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
      return sendError(reply, req.log, error, `emu file ${name}`);
    }
  });
}

function sendError(reply: FastifyReply, log: FastifyBaseLogger, error: unknown, context: string): FastifyReply {
  if (error instanceof GitHubError) {
    const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
    log.warn({ context, githubStatus: error.status }, "GitHub error while serving the emulator payload.");
    return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
  }
  log.error({ err: error, context }, "Unexpected emu route error.");
  return reply.code(502).send({ error: "upstream_error" });
}
