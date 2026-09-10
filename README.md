# Drydock

A native desktop tool for managing a Steam game library — installs, depot downloads, per-game
configuration and a local emulator toolchain — backed by a proxy you can run yourself.

Written in Rust. No account credentials are ever handled: Drydock never asks for, stores or
transmits a Steam login.

```
apps/drydock-desktop   egui/eframe desktop shell        → binary: Drydock
crates/drydock-core    portable core: all logic, I/O, networking, crypto
proxy/                 Fastify service in front of the upstream APIs (deployed separately)
```

## What it does

- **Library** — discovers games installed by Steam and by Drydock, grouped and searchable, with
  per-category actions (install, verify, update, uninstall, launch).
- **Depot downloads** — a parallel, resumable download engine that verifies every chunk against its
  manifest checksum and repairs damaged installs in place.
- **Store** — browse featured, new releases, repacks and Denuvo-tracked titles, filtered against the
  available catalog.
- **Emulator toolchain** — generates a complete local configuration (DLCs, languages, achievements
  with mirrored icons) and deploys the matching binaries for the detected architecture.
- **Cloud saves** — optional CloudRedirect integration with folder, S3/R2, Google Drive and OneDrive
  providers.
- **Self-updating** — SHA-256-verified releases for Windows and Linux on x64 and ARM64.

## Install

Download the binary for your platform from the
[releases page](https://github.com/NhMarco/Drydock/releases) along with its `.sha256` file, verify
it, and run it. There is no installer; the executable is self-contained.

```powershell
Get-FileHash .\Drydock-windows-x64.exe -Algorithm SHA256
```

Per-user data lives in `%LOCALAPPDATA%\Drydock` on Windows and `$XDG_DATA_HOME/Drydock` elsewhere.
Regenerable caches sit in a `cache/` subfolder and can be deleted at any time.

## Build from source

Requires Rust 1.88 or newer.

```powershell
cargo run -p drydock-desktop
```

A fresh clone builds and starts with no configuration at all. It reports the proxy as unconfigured
and the local features — library, emulator toolchain, folder detection — still work. Point it at a
proxy to enable the rest.

## Configuration

Every value a deployment can change is resolved by `drydock-core::config` from three layers,
**highest priority first**:

| Layer | Where | Use it for |
| --- | --- | --- |
| 1. Environment variable | the process environment | scripting, CI, development |
| 2. Settings | Settings ▸ **Proxy / Self-hosting**, stored in `settings.json` | pointing a released build at your own proxy, without rebuilding |
| 3. Build-time default | `build.rs`, from an env var or a git-ignored `*.secret` file | what official release builds ship with |

A blank value at one layer falls through to the next rather than blanking out what is below it.

| Variable | Meaning |
| --- | --- |
| `DRYDOCK_PROXY_BASE_URL` | Proxy origin, e.g. `https://proxy.example` — origin only, no path or trailing slash |
| `DRYDOCK_HMAC_SECRET` | Shared secret; must match one of the proxy's `DRYDOCK_HMAC_SECRET` values |
| `DRYDOCK_UPDATE_REPOSITORY` | `owner/repo` the self-updater checks. Set this if you fork and publish your own releases |
| `DRYDOCK_GITHUB_TOKEN` | Rarely needed — see below |
| `DRYDOCK_RELEASE_VERSION` | Version the binary reports. The release workflow sets it from the git tag |

**Drydock needs no GitHub token.** The client's only GitHub traffic is the self-updater asking the
Releases API for the latest version, which works anonymously against a public repository.
`DRYDOCK_GITHUB_TOKEN` exists for two edge cases: hitting the anonymous rate limit (60 requests per
hour per IP), or pointing the updater at a *private* release repository. The token that reads the
payload repository belongs to the proxy, not to the client — it never ships in the binary.

To see what actually resolved and which layer it came from — secrets are redacted, so the output is
safe to paste into an issue:

```powershell
cargo run -p drydock-desktop -- --config
```

### Running your own proxy

The proxy is a self-contained Fastify service in [`proxy/`](proxy/). It holds the upstream API keys
so they never ship inside the client, and it verifies an HMAC signature on every request.

```bash
cd proxy
cp .env.example .env          # fill in the upstream keys
openssl rand -hex 32          # generate DRYDOCK_HMAC_SECRET
docker compose up -d --build
```

Put the same secret and your proxy's URL into Drydock under Settings ▸ Proxy / Self-hosting, or set
the environment variables. [`proxy/README.md`](proxy/README.md) documents the endpoints and the
request-signing scheme.

To bake your own defaults into a binary instead:

```powershell
$env:DRYDOCK_PROXY_BASE_URL = "https://proxy.example"
$env:DRYDOCK_HMAC_SECRET    = "…"
cargo build -p drydock-desktop --release
```

## Development

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p drydock-desktop -- --self-test
```

Clippy runs with `-D warnings`; `rustfmt.toml` sets `max_width = 110`. New logic belongs in
`drydock-core` with tests beside it rather than in the UI layer.
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) covers the module layout and the conventions that are
not obvious from the code.

On Windows the full release gate additionally waits for the GUI-subsystem executable and validates
its exit code, output, version, size and SHA-256:

```powershell
.\tools\verify-release.ps1 -ExpectedVersion 1.0.0
```

### Diagnostic flags

| Flag | Purpose |
| --- | --- |
| `--self-test` | Headless integrity check; the release gate |
| `--config` | Print the resolved configuration and each value's source |
| `--version` | Print the version this binary reports |
| `--check-update` | Full download + verify + stage, without applying |

## Build targets

Each operating system and CPU architecture gets its own native executable; there is no single format
that runs unchanged on Windows and Linux.

- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

GitHub Actions builds and validates all four on native x64/ARM64 runners. A tag matching
`vMAJOR.MINOR.PATCH` publishes a release with one standalone executable and `.sha256` per target. The
tag version is compiled into every official binary and verified before publishing, and each packaged
binary must pass its own headless `--self-test` on the native runner before it is uploaded.

## Security

The client ships no upstream API key and no GitHub token — only a proxy address and a shared signing
secret. Report security issues privately rather than opening a public issue; see
[SECURITY.md](SECURITY.md).

## License

[GPL-2.0-only](LICENSE).
