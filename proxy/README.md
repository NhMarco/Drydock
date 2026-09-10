# Drydock Proxy

A small, hardened reverse proxy that sits between the Drydock app and the upstream data providers.

## Providers

The gamelist, the per-app Lua and the depot packages all come from **providers**, selected by one
ordered list in `PROVIDER_SOURCES`. Order is priority: the first provider wins on shared App IDs and
is tried first; the others fill what it is missing, so one being down does not take the proxy with it.

| Provider | Credential | Notes |
| --- | --- | --- |
| `ryu` | `RYU_AUTH_CODE` | **The default.** `generator.ryuu.lol`; `secure_download` returns the manifests+lua ZIP |
| `depotbox` | `DEPOTBOX_API_KEY` | `depotbox.org`; also the source for the per-variant game fixes |
| `steamtools` | `STEAMTOOLS_API_KEY` | `api.steamtools.app`; also backs the on-demand `/v1/app-info` enrichment |

`PROVIDER_SOURCES` defaults to `ryu` alone. A provider that is not listed is fully off and is never
contacted — and **its credential is not required to boot**, so running Ryu-only needs no SteamTools
or DepotBox key. Enable more by listing them, e.g. `PROVIDER_SOURCES=ryu,depotbox,steamtools`.

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
| GET    | `/v1/app-info/:appid`   | yes  | steamtools per-app metadata (type, technologies incl. Denuvo, tags, reviews). Cached. |
| GET    | `/v1/app-schema/:appid` | yes  | The app's achievement schema as a gbe_fork `achievements.json` array (icons as full CDN URLs), for the emulator-template generator. Needs `STEAM_WEB_API_KEY`; returns `[]` when unset or the app has none. |
| GET    | `/v1/fixes`             | yes  | `{ "fixes": [{ "appid", "name", "variants": [{ "id", "filename", "tags", "badges" }] }] }` — DepotBox "online" fixes. |
| GET    | `/v1/fixes/file/:id`    | yes  | One online-fix variant as a ZIP, converted server-side from the upstream DepotBox RAR. |
| GET    | `/v1/denuvo-fixes`      | yes  | `{ "fixes": [{ "appid", "lua": {name,sha,size}, "zip_parts": [{name,sha,size}] }] }` — GitHub build-locked "Denuvo" fixes. |
| GET    | `/v1/denuvo-fixes/file/:name` | yes | Raw bytes of one Denuvo fix file (`{appid}.lua`, `{appid}.zip`, `{appid}.zip.NNN`), streamed from GitHub. |
| GET    | `/v1/repacks`           | yes  | `{ "repacks": [{ "appid", "sources": [{ "repacker", "link" }] }] }`. External http(s) download links; no file bytes. |
| GET    | `/v1/depot/package/:appid` | yes | `application/zip` of the app's depot `.manifest` files plus the depot key (an `<appid>.lua` from Ryu/SteamTools, or a `.key` file from DepotBox). Streamed. `X-Depot-Source` names the upstream that served it. |

`/v1/depot/package/:appid` relays the per-app depot package Drydock uses to download real game files
itself (manifest + depot key → Steam CDN). It draws from one or more upstreams — **Ryu**
(`secure_download`), **DepotBox** (`direct-download`), **SteamTools** (`manifest`) — each returning a
manifests+keys ZIP. The enabled set and try order come from **`DEPOT_PACKAGE_SOURCES`** (comma-
separated, first match wins; unknown names ignored). It defaults to `ryu`; set it to
`ryu,depotbox,steamtools` to use all three again. Depot keys are read from the bundled `.lua`
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
