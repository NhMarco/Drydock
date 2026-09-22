# Drydock Proxy

A small, hardened reverse proxy that sits between the Drydock app and the upstream data providers.

## Providers

The gamelist, the per-app Lua and the depot packages all come from **providers**, selected by one
ordered list in `PROVIDER_SOURCES`. Order is priority: the first provider wins on shared App IDs and
is tried first; the others fill what it is missing, so one being down does not take the proxy with it.

| Provider | Credential | Notes |
| --- | --- | --- |
| `ryu` | `RYU_AUTH_CODE` | **The default.** `generator.ryuu.lol`; `secure_download` returns the manifests+lua ZIP |
| `depotbox` | `DEPOTBOX_API_KEY` | `depotbox.org`; builds Lua on demand (`DEPOTBOX_LUA_TIMEOUT_MS`, default 90 s) and lists no tags |
| `steamtools` | `STEAMTOOLS_API_KEY` | `api.steamtools.app`; also backs `/v1/app-info`. The key allows 85 requests/min plus a daily quota |
| `hubcap` | `HUBCAP_API_KEY` | `hubcapmanifest.com`; **day-limited** — see below. Its library (~157k apps) feeds the gamelist for free |

`PROVIDER_SOURCES` defaults to `ryu` alone. A provider that is not listed is fully off and is never
contacted — and **its credential is not required to boot**, so running Ryu-only needs no SteamTools
or DepotBox key. Enable more by listing them, e.g. `PROVIDER_SOURCES=ryu,depotbox,steamtools`.

A provider that answers `429` is skipped for Lua and depot requests until its limit resets, a
depot package is only cached once it is a real ZIP archive (an HTML or JSON answer moves on to the
next provider), and provider credentials are never forwarded when an upstream redirects to another
host. When a provider without tags (DepotBox) wins an App ID, the tags another provider had are kept.

### Day-limited providers (Hubcap)

Hubcap's manifest downloads are capped **per day** (the others are capped per minute at worst), so it
is treated differently from the rest — wherever it appears in `PROVIDER_SOURCES`:

- **Asked last, and only when it is the only option.** Every ordinary provider is tried first; then
  any copy already on disk, however old, is used instead. Only when there is neither is Hubcap asked.
- **Never asked without allowance.** `/api/v1/generate/usage` reports what is left of the day (the
  `single` bucket, cached for `HUBCAP_USAGE_TTL_SECONDS`); at or below `HUBCAP_RESERVE` the provider
  is skipped. A refresh (`force_update`) is never requested — that is what spends the allowance.
- **What it serves is kept.** Its package is stored as a *kept* cache entry: it outlives the 24 h TTL
  and the eviction sweep, so the app stays downloadable without spending the allowance again. The
  moment an ordinary provider serves that app, its package replaces the kept one and normal caching
  resumes. `X-Cache: KEPT` marks a response that came from such an entry.
- **No Lua.** Hubcap's unlock only exists inside the manifest ZIP, so `/v1/lua/:appid` never uses it;
  fetching a whole package to answer a Lua request would spend a day's allowance on it.
- **Gamelist at the lowest priority.** Its library is free to read and is merged in last, so it only
  adds the apps no other provider lists (and, having no tags, never overwrites another provider's).

A second group of payloads — the Steam Service (OST) files, the Denuvo fixes, the repacks list and
the Ubisoft magicfiles — is served from a GitHub repository (`GITHUB_OWNER`/`GITHUB_REPO`) rather
than from a provider. Those endpoints need `GITHUB_TOKEN` when that repository is private; without
it the proxy still starts and everything else keeps working, with a warning at boot.

Why the proxy exists:

- **No upstream API key ever ships in the app.** They live only in the proxy's environment.
- **The gamelist (~35 MB, ~246k apps) is fetched once per hour**, compacted to
  `{ appid, name, tags }` (NSFW entries filtered, `tag:` prefixes stripped), gzipped, and
  served to every client from cache — clients never hit the upstream gamelist endpoint.
- **Type/DRM is not in the bulk gamelist**, so the client enriches on demand per app via
  `/v1/app-info/:appid` (steamtools `app-info`, cached).
- **Requests are HMAC-signed**, so a random script without the app's embedded secret is
  rejected, and a leaked secret can be revoked by rotation.
- **The per-app fixes are pulled from GitHub by the proxy** (via the GitHub **Contents API**,
  matching `crates/drydock-core/src/mfb.rs`), so clients fetch them from the proxy instead of GitHub
  and the app no longer needs an embedded GitHub token. File bytes are streamed straight through
  (so 100 MB fix zip parts are never buffered); the proxy advertises each file's git-blob sha and
  the client verifies it.

- **Per-app "repacks" are a hand-maintained list, not files.** `Files/repacks.json` in the MFB
  repo maps each App ID to one or more external download sources
  (`{ "<appid>": [{ "repacker": "FitGirl", "link": "https://…" }] }`). The proxy validates it
  (numeric App IDs, http(s) links only), normalizes it to a stable array, and caches it. The client
  shows these in a Repacks tab and opens the chosen link in the user's browser — non-http(s) links
  are dropped on both sides so a bad entry can never open a dangerous URI.

## Endpoints

All responses are JSON unless noted. Protected endpoints require the HMAC headers below.

| Method | Path             | Auth | Notes |
|--------|------------------|------|-------|
| GET    | `/v1/health`            | no   | Liveness + whether the gamelist is loaded. |
| GET    | `/v1/gamelist`          | yes  | gzip body of `{ "games": [{ "appid", "name", "tags" }] }`. Send `If-None-Match` with the last `ETag` to get `304`. |
| GET    | `/v1/app-info/:appid`   | yes  | steamtools per-app metadata (type, technologies incl. Denuvo, tags, reviews). Cached. Only registered while `steamtools` is enabled; limited like `/v1/lua`. |
| GET    | `/v1/app-schema/:appid` | yes  | The app's achievement schema as a gbe_fork `achievements.json` array (icons as full CDN URLs), for the emulator-template generator. Needs `STEAM_WEB_API_KEY`; returns `[]` when unset or the app has none. |
| GET    | `/v1/denuvo-fixes`      | yes  | `{ "fixes": [{ "appid", "lua": {name,sha,size}, "zip_parts": [{name,sha,size}] }] }` — GitHub build-locked "Denuvo" fixes. |
| GET    | `/v1/denuvo-fixes/file/:name` | yes | Raw bytes of one Denuvo fix file (`{appid}.lua`, `{appid}.zip`, `{appid}.zip.NNN`), streamed from GitHub. |
| GET    | `/v1/lua/:appid`        | yes  | The app's unlock Lua from the first provider that has one. 30 requests/min per client (`LUA_RATE_MAX`). |
| GET    | `/v1/service/manifest`, `/v1/service/file/:name` | yes | Steam Service (OST) payload from GitHub. |
| GET    | `/v1/emu/manifest`, `/v1/emu/file/:name` | yes | Emulator DLLs from GitHub (`EMU_DIRECTORY`, default `Files/dlls`). |
| GET    | `/v1/repacks`           | yes  | `{ "repacks": [{ "appid", "sources": [{ "repacker", "link" }] }] }`. External http(s) download links; no file bytes. |
| GET    | `/v1/depot/package/:appid` | yes | `application/zip` of the app's depot `.manifest` files plus the depot keys (an `<appid>.lua` with keyed `addappid` lines, from every provider). Streamed. `X-Depot-Source` names the upstream that served it, `X-Cache` is `MISS`/`HIT`/`KEPT`. |

`/v1/depot/package/:appid` relays the per-app depot package Drydock uses to download real game files
itself (manifest + depot key → Steam CDN). It draws from one or more upstreams — **Ryu**
(`secure_download`), **DepotBox** (`direct-download`), **SteamTools** (`manifest`), **Hubcap**
(`api/v1/manifest`) — each returning a manifests+keys ZIP. The enabled set and try order come from **`PROVIDER_SOURCES`** (comma-separated,
first valid ZIP wins; unknown names ignored; `DEPOT_PACKAGE_SOURCES` is the legacy fallback). It
defaults to `ryu`; set it to `ryu,depotbox,steamtools` to use all three. Depot keys are read from the bundled `.lua`
(`addappid(<depotid>, 0|1, "<hexkey>")`) or `.key` file. Ryu needs `RYU_AUTH_CODE` (its reseller auth
code, sent as a query parameter); DepotBox/SteamTools use their existing `DEPOTBOX_API_KEY` /
`STEAMTOOLS_API_KEY`.

`/v1/gamelist` sets `Content-Encoding: gzip` and returns the compressed bytes directly, plus
`ETag`, `X-Gamelist-Count`, and `X-Gamelist-Updated` (Unix seconds). Fix file responses set
`X-Content-Git-Sha` so the client can verify against the manifest.

## Authentication (HMAC)

Every protected request must carry three headers:

| Header              | Value |
|---------------------|-------|
| `X-Drydock-Timestamp` | Unix seconds when the request was signed. |
| `X-Drydock-Nonce`     | Unique random token per request (`[A-Za-z0-9_-]{16,64}`). |
| `X-Drydock-Signature` | Base64 `HMAC-SHA256(secret, signingString)`. |

The **signing string** is exactly these four parts joined by `\n` (LF), no trailing newline:

```
METHOD \n PATH_AND_QUERY \n TIMESTAMP \n NONCE
```

- `METHOD` — upper-case (`GET`).
- `PATH_AND_QUERY` — the request target as sent, e.g. `/v1/app-info/730` (include `?query` if present).
- `TIMESTAMP` / `NONCE` — identical to the header values.

The proxy rejects the request (`401`) if the signature is invalid, the timestamp is outside
`HMAC_WINDOW_SECONDS` (default ±60 s), or the nonce was already used (replay).

### Known-answer test vector

Use this to confirm any implementation (the TS verifier, the C# client) produces identical
bytes:

```
secret         = test-secret
method         = GET
pathAndQuery   = /v1/app-info/730
timestamp      = 1700000000
nonce          = abc123def4567890
signingString  = "GET\n/v1/app-info/730\n1700000000\nabc123def4567890"
signature      = l+/pk0jM9UXgbgGYkAfnE+IS3yJ51BvqxMv8MMoY5cs=
```

Reference implementation and a live tester: [`scripts/sign.mjs`](scripts/sign.mjs).

```bash
DRYDOCK_HMAC_SECRET=yoursecret node scripts/sign.mjs GET /v1/app-info/730 http://localhost:8080 --send
```

## Configuration

Copy `.env.example` to `.env`. What you must set depends on which providers you enable:

- **Always:** `DRYDOCK_HMAC_SECRET` — the shared signing secret, also entered in the Drydock client.
- **Per active provider:** its credential from the table above. With the default
  `PROVIDER_SOURCES=ryu` that is `RYU_AUTH_CODE` and nothing else.
- **For the GitHub-hosted payloads:** `GITHUB_TOKEN`, a read-only PAT, when that repository is
  private. Optional — the proxy boots without it and warns.

Everything else has a documented default. All variables are listed in `.env.example` and validated
at boot (`src/config.ts`), which fails fast on a missing credential for an *enabled* provider.

Generate a secret:

```bash
openssl rand -hex 32
```

## Run

### Docker (recommended)

```bash
cp .env.example .env      # then edit .env
docker compose up -d --build
docker compose logs -f
```

The gamelist cache is stored in the `gamelist-data` volume, so restarts serve data
immediately. Put the container behind a TLS reverse proxy — see `Caddyfile.example`. Keep the
Node service off the public internet (the compose file binds it to `127.0.0.1`).

### Local (Node 20+)

```bash
npm install
cp .env.example .env      # then edit .env
npm run dev               # watch mode
# or
npm run build && npm start
```

For quick local testing without signing, set `REQUIRE_AUTH=false` in `.env` (never in
production).

## Rotating secrets

`DRYDOCK_HMAC_SECRET` accepts a comma-separated list. To roll out a new secret without breaking
already-published app builds, set both old and new (`old,new`), ship the new app build, then
drop the old value on the next deploy. The same applies to the upstream key: change
`STEAMTOOLS_API_KEY` and restart — no client change needed.
