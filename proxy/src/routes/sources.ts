// Which provider has an app, and how current its unlock is — what the app lists when the user picks a
// source for "Add to Steam" or a download.
//
//   GET /v1/sources/:appid -> { sources: [{ name, listed, updated }] }
//
// `sources` follows `PROVIDER_SOURCES` (the order "automatic" tries them in). `listed` is `null` for a
// provider whose list names every Steam app (SteamTools), so it cannot say; `updated` is Unix seconds
// where the provider's list dates its unlocks (Ryu), else `null`. A provider not indexed yet (the
// gamelist has not been fetched since start) is reported as `listed: null` too.

import type { FastifyInstance, preHandlerHookHandler } from "fastify";
import type { DepotPackageSourceName } from "../config.js";
import type { ProviderIndex } from "../providerIndex.js";

const APPID_PATTERN = /^[1-9][0-9]{0,9}$/;

export function registerSourcesRoute(
  app: FastifyInstance,
  providers: readonly DepotPackageSourceName[],
  index: ProviderIndex,
  authHook: preHandlerHookHandler,
): void {
  app.get<{ Params: { appid: string } }>("/v1/sources/:appid", { preHandler: authHook }, async (req, reply) => {
    const { appid } = req.params;
    if (!APPID_PATTERN.test(appid)) return reply.code(400).send({ error: "invalid_appid" });
    const id = Number.parseInt(appid, 10);
    const sources = providers.map((name) => {
      const answer = index.lookup(name, id);
      return { name, listed: answer?.listed ?? null, updated: answer?.updated ?? null };
    });
    return reply.code(200).header("Cache-Control", "no-store").send({ sources });
  });
}
