// Serves the per-app Ubisoft "magicfiles" payload (the DRM helper files Drydock places next to the
// game exe before the first launch) through the proxy, streamed straight from GitHub so the app
// needs no embedded GitHub token.
//
//   GET /v1/magicfiles/:appid  -> the {appid}.zip bytes (streamed), or 404 when no magicfiles
//                                 exist for that app (so the client can say "not available").
//
// Mirrors the Steam Service file route: the bytes are streamed fresh from GitHub on each request
// and never buffered in the proxy.

import type { FastifyInstance, FastifyBaseLogger, FastifyReply, preHandlerHookHandler } from "fastify";
import { GitHubError, type GitHubClient } from "../github.js";
import type { Config } from "../config.js";
import type { FileCache } from "../fileCache.js";

const APPID_PATTERN = /^[0-9]{1,10}$/;

export function registerMagicfilesRoute(
  app: FastifyInstance,
  config: Config,
  github: GitHubClient,
  fileCache: FileCache,
  authHook: preHandlerHookHandler,
): void {
  app.get<{ Params: { appid: string } }>(
    "/v1/magicfiles/:appid",
    { preHandler: authHook },
    async (req, reply) => {
      const { appid } = req.params;
      if (!APPID_PATTERN.test(appid)) return reply.code(400).send({ error: "invalid_appid" });
      const disposition = `attachment; filename="${appid}.zip"`;
      const path = `${config.magicfilesDirectory}/${appid}.zip`;
      try {
        // Cached 24h so the same magicfiles payload is pulled from GitHub at most once a day, and
        // concurrent misses for the same app share one upstream fetch.
        //
        // There is deliberately no "cache write failed, pipe the upstream through instead" fallback:
        // a failed `put` has already consumed part of `raw.stream`, so replaying it would send a
        // truncated ZIP under a correct-looking Content-Length. An error the client can retry is the
        // only honest answer.
        const { hit, cached } = await fileCache.getOrFetch(
          `magicfiles:${appid}`,
          "application/zip",
          async () => (await github.openRawStream(path)).stream,
        );
        return reply
          .header("Content-Type", "application/zip")
          .header("Cache-Control", "no-store")
          .header("X-Cache", cached ? "HIT" : "MISS")
          .header("Content-Disposition", disposition)
          .header("Content-Length", String(hit.contentLength))
          .send(hit.stream());
      } catch (error) {
        return sendError(reply, req.log, error, `magicfiles ${appid}`);
      }
    },
  );
}

function sendError(reply: FastifyReply, log: FastifyBaseLogger, error: unknown, context: string): FastifyReply {
  if (error instanceof GitHubError) {
    const status = error.status === 404 ? 404 : error.status === 504 ? 504 : 502;
    log.warn({ context, githubStatus: error.status }, "GitHub error while serving magicfiles payload.");
    return reply.code(status).send({ error: status === 404 ? "not_found" : "upstream_error" });
  }
  log.error({ err: error, context }, "Unexpected magicfiles route error.");
  return reply.code(502).send({ error: "upstream_error" });
}
