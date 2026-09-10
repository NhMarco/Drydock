# Architecture

How Drydock is put together, and the conventions that are not obvious from reading the code.

## Layout

| Path | Role |
| --- | --- |
| `crates/drydock-core` | The portable core: all business logic, I/O, networking and crypto. No UI. Every non-trivial change should land here, with tests. |
| `apps/drydock-desktop` | The `egui`/`eframe` desktop shell (binary `Drydock`). A thin layer over the core; most of it lives in one large `src/ui.rs`. |
| `proxy/` | A Fastify (Node 20+, TypeScript) service in front of the upstream APIs. Deployed independently via Docker; not built by Cargo. |

## Core modules

`lib.rs` re-exports the public surface. The main ones:

- `activation` — machine- and App-ID-bound encrypted request codes, plus signed response verification.
- `steam`, `steam_process`, `steam_service`, `steam_uri` — library discovery, Valve KeyValues manifest
  parsing, graceful restart, the Steam Service payload.
- `depot/` — the download engine: manifest parsing, chunk decryption, CDN rotation, the parallel
  resumable downloader and the verifier.
- `catalog`, `store`, `fixes`, `denuvo`, `language`, `repacks` — game data and per-app state.
- `emu_template`, `emu_toolchain`, `emu_load_dlls` — the local emulator configuration generator and
  its binary toolchain.
- `cloud` — CloudRedirect DLL deployment and provider configuration, including the OAuth loopback flow.
- `proxy` — the HMAC client for the self-hosted proxy.
- `updater` — the SHA-256-verified GitHub Release self-updater.
- `paths`, `settings`, `config`, `self_test`, `version`, `harden`, `administrator`, `conflicts`.

## Conventions that matter

### Configuration goes through `config.rs`

Never read a deployment-changeable value directly. `drydock-core::config` resolves each one from
three layers, highest first: **environment variable** → **user settings** (the Settings ▸ Proxy /
Self-hosting fields, pushed in by `Settings::apply_config_overrides`) → **build-time default**
(`build.rs` `cargo:rustc-env`, from an env var or a git-ignored `*.secret` file).

New configuration belongs in that module with a `describe` entry, so `--config` keeps showing the
whole picture. `proxy.rs` and `updater.rs` only re-export from it. A missing value resolves to `None`
and callers must degrade gracefully — a fresh clone with no secrets has to reach a usable window.

Two traps:

- Cargo passes `cargo:rustc-env` values into the environment of binaries it launches, so under
  `cargo run` / `cargo test` the build-time default *also* occupies the environment layer. A directly
  launched binary does not see them. Tests that exercise layer precedence must clear the variable
  first — see `config::tests::user_overrides_beat_the_build_default`.
- `build.rs` must declare `cargo:rerun-if-env-changed` for anything read with `option_env!`, or cargo
  will reuse a crate compiled against a different value.

### Writing remote-sourced paths: always use `safe_path`

Depot manifests, fix archives and emulator skeleton ZIPs all carry relative paths that get joined
onto a target folder. **Every** such write goes through `is_safe_path_segment` / `join_within`.

Filtering `..` alone is not sufficient on Windows: `PathBuf::push("C:")` replaces the entire
accumulated path rather than appending to it, so a manifest entry of `C:/Windows/…` escapes the
install root. `capacity_hint` likewise clamps an archive's self-declared entry size before it reaches
`Vec::with_capacity`, so a crafted header cannot demand an arbitrary allocation.

### Settings are loaded recoveringly

Use `Settings::load_recovering`, not `Settings::load`. A corrupt or half-written `settings.json`
falls back to the `.bak` copy that `save` leaves behind, and an unsalvageable file is quarantined
rather than replaced.

The returned `LoadOutcome::save_is_safe()` gates writing. When it is false the UI must block saves:
the in-memory settings hold defaults while the user's real `added_apps` / `installed_games` /
`launch_paths` may still be in the file on disk, and writing would destroy them.

### Keep logic out of the UI

`ui.rs` is large and hard to test. Two modules exist specifically to pull logic out of it, and new
code should use them rather than re-implementing their rules inline:

- `download_queue` — the persistent download queue's ordering rules as pure functions returning a
  `QueueEffect` the UI then acts on.
- `safe_path` — as above.

### Persistence

`PortablePaths` resolves `%LOCALAPPDATA%\Drydock` (Windows) or `$XDG_DATA_HOME/Drydock` (elsewhere).
Persistent data — settings, the activation device key — lives in the root; regenerable caches live in
`cache/` and can be wiped safely. Both the image cache and the store cache sweep themselves on
startup, so neither grows without bound.

## The proxy

The client talks **only** to the proxy. No upstream API key and no GitHub token ships in the binary —
only the proxy base URL and a shared HMAC secret. The proxy holds the real credentials.

Requests are signed as `METHOD\nPATH_AND_QUERY\nTIMESTAMP\nNONCE` (LF-separated, no trailing
newline), sent as `X-Drydock-Timestamp`, `X-Drydock-Nonce` and `X-Drydock-Signature`. The HMAC and
SHA-256 logic, and the known-answer test vector, must stay identical between `proxy/src/hmac.ts` and
`crates/drydock-core/src/proxy.rs` — both sides have a test pinning the same vector.

The gamelist is merged from the configured upstreams, cached hourly and served gzipped. Larger
user-facing files (depot packages, magicfiles) go through a disk cache with a TTL and a periodic
eviction sweep, and concurrent misses for the same key collapse into a single upstream fetch.

`proxy/README.md` documents the endpoints; `proxy/.env.example` documents the configuration.

## Versioning and releases

The single source of truth for the version is `[workspace.package] version` in the root
`Cargo.toml`. Official binaries take their version from the release tag at build time, injected as
`DRYDOCK_RELEASE_VERSION`, not from that constant.

Building locally with `DRYDOCK_RELEASE_VERSION=9.9.9` produces a binary that outranks every published
release, which stops the self-updater from replacing your own build while testing.

CI (`.github/workflows/ci.yml`) is **manual-only** (`workflow_dispatch`) to conserve minutes — do not
assume a push runs it. `release.yml` runs on `vMAJOR.MINOR.PATCH` tags and builds four native
targets, each self-tested before upload.

## Known limitations

- The self-updater verifies a SHA-256 that ships from the same GitHub Release as the binary. That
  defends against a corrupted or truncated download, but not against whoever controls the release.
  Closing that gap needs a signature verified against a key the client already holds — Authenticode,
  or an embedded public key over a detached signature. Until then the release credentials are the
  security boundary.
- Crash reports (`crash.log` in the data directory) capture the panic message and its exact
  `file:line:column`, but not symbolised application frames: release builds ship without PDBs.
