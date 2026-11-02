# Developer guide

Everything about building, configuring and releasing Drydock. To simply use the app, see the
[README](../README.md).

```
apps/drydock-desktop   egui/eframe desktop shell        → binary: Drydock
crates/drydock-core    portable core: all logic, I/O, networking, crypto
proxy/                 Fastify service in front of the upstream APIs (deployed separately)
```

New logic belongs in `drydock-core`, with tests beside it, rather than in the UI layer.
[ARCHITECTURE.md](ARCHITECTURE.md) covers the module layout and the conventions that are not obvious
from the code. Drydock can also be built under another name and look — a white-label build is this
repository plus one `brand/` folder; see [branding.md](branding.md).

## Build and run

Requires **Rust 1.95+**. Windows builds need the MSVC C++ build tools and the Windows SDK. On
Ubuntu/Debian, install the Linux build dependencies:

```bash
sudo apt-get install libgl1-mesa-dev libwayland-dev libx11-dev libxkbcommon-dev libxi-dev pkg-config
```

From the repository root, run the app for development, or build a release executable:

```bash
cargo run -p drydock-desktop
```

```bash
cargo build --locked --release -p drydock-desktop
```

The built app is `target/release/Drydock.exe` on Windows or `target/release/Drydock` on Linux.

A fresh clone builds and starts with no configuration at all. It reports the proxy as unconfigured,
and the local features — library, emulator toolchain, folder detection — still work. Point it at a
proxy (below) to enable the rest.

## Configuration

Every value a deployment can change is resolved by `drydock-core::config` from two layers,
**highest priority first**:

| Layer | Where | Use it for |
| --- | --- | --- |
| 1. Environment variable | the process environment | scripting, CI, development |
| 2. Build-time default | `build.rs`, from an env var or a git-ignored `*.secret` file | what official release builds ship with |

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

```bash
cargo run -p drydock-desktop -- --config
```

## Running your own proxy

The proxy is a self-contained Fastify service in [`proxy/`](../proxy/) (Node.js 24+). It holds the
upstream API keys so they never ship inside the client, verifies an HMAC signature on every request
and can require a Discord sign-in.

```bash
cd proxy
cp .env.example .env          # fill in the upstream keys
openssl rand -hex 32          # generate DRYDOCK_HMAC_SECRET
docker compose up -d --build
```

Build Drydock with the same secret and your proxy's URL, or set the environment variables when you
run it. [`proxy/README.md`](../proxy/README.md) documents the providers, the endpoints, the Discord
login and the request-signing scheme.

To bake your own defaults into a binary instead:

```powershell
$env:DRYDOCK_PROXY_BASE_URL = "https://proxy.example"
$env:DRYDOCK_HMAC_SECRET    = "…"
cargo build -p drydock-desktop --release
```

## Checks

```bash
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p drydock-desktop -- --self-test
```

Clippy runs with `-D warnings`; `rustfmt.toml` sets `max_width = 110`. CI also builds and tests once
with the example brand (`DRYDOCK_BRAND_DIR=docs/brand-template`), so every optional feature is built
both ways. For the proxy, run `npm test` in `proxy/`.

On Windows the full release gate additionally waits for the GUI-subsystem executable and validates its
exit code, output, version, size and SHA-256:

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

## Build targets and releases

Each operating system and CPU architecture gets its own native executable; there is no single format
that runs unchanged on Windows and Linux:

- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

Maintainers publish a release by pushing a `vMAJOR.MINOR.PATCH` tag on a commit contained in `main`;
a push to `main` alone publishes nothing. The [release workflow](../.github/workflows/release.yml)
validates the code, builds every target on native x64/ARM64 runners, and publishes one standalone
executable and `.sha256` per target. The tag version is compiled into every official binary and
verified before publishing, and each packaged binary must pass its own headless `--self-test` on its
runner before it is uploaded.

## Security

The client ships no upstream API key and no GitHub token — only a proxy address and a shared signing
secret. A user's own Hubcap Manifest key stays on their computer (DPAPI-sealed on Windows) and is never
sent to the proxy. Report security issues privately rather than opening a public issue; see
[SECURITY.md](../SECURITY.md).
