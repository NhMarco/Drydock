# White-label builds

Drydock can be built under another name and look — a white-label product. Such a product is **not
a fork**: its repository is this one, unchanged, plus a single `brand/` folder at the root. Drydock
itself has no `brand/` folder and contains nothing of any other product.

Because the product never edits a file that Drydock has, taking over every Drydock update is one
conflict-free pull (see [Keeping it current](#keeping-it-current)).

## What a brand changes, and what it never does

| Set by `brand/brand.toml` | Stays Drydock's in every product |
| --- | --- |
| Name in the window, headings and messages | The proxy it talks to and the signed requests (`X-Drydock-*`) |
| Every colour of the interface (`[palette]`) | The `settings.json` format |
| Logo, window icon, Windows file icon (`brand/*.png`, `brand/app-icon.ico`) | Activation: request codes, response verification, the `.Drydock` marker |
| Navigation: top bar or left column | The depot engine, downloads, verify, Steam integration |
| Optional features: Tools, Cloud, Repacks, Denuvo fix, cracked version | `DRYDOCK_*` environment variables, the user agent |
| Release assets (`<name>-windows-x64.exe`, …) and the Windows file details | The crate and binary names inside the build (`target/release/Drydock.exe`) |
| Update channel: the product repository's own releases | |
| Data directory: `%LOCALAPPDATA%\<name>` (settings, library, cache) | |

Because the backend is identical, a white-label build and Drydock are interchangeable towards the
proxy, the activation bot and Steam. Each keeps its own data directory, so both can live on one
machine without seeing or clearing each other's settings, library or cache.

## The brand folder

```
brand/
├── brand.toml     required — name, colours, navigation, features
├── logo.png       optional — the logo in the navigation (square, transparent outside the mark)
├── app-icon.png   optional — the window and taskbar icon (256 px)
└── app-icon.ico   optional — the Windows file icon (16–256 px)
```

Missing artwork falls back to Drydock's. [`docs/brand-template/brand.toml`](brand-template/brand.toml)
is a complete, commented example: copy the folder to `brand/` and edit it.

### `brand.toml`

| Key | Meaning | Default |
| --- | --- | --- |
| `name` | The product's name; also its release assets and data directory. A letter first, then letters, digits, `-` or `_`, at most 32 characters. | required |
| `tagline` | A short line under the name in the side navigation. | Drydock's |
| `description` | Explorer's "File description". | `<name> — <tagline>` |
| `logo_is_wordmark` | `true` when `logo.png` already spells the name, so the side navigation shows the logo alone. | `false` |
| `navigation` | `"top"` or `"side"`. | `"top"` |
| `[features]` | `tools`, `cloud`, `repacks`, `denuvo_fix`, `cracked_version` — each `true` or `false`. | all on |
| `[palette]` | `background`, `surface`, `surface_raised`, `surface_sunken`, `border`, `edge`, `text`, `muted`, `accent`, `accent_soft`, `accent_deep`, `success`, `success_hover`, `on_success`, `warning`, `danger`, `chrome`, `scrim`, `overlay`, `input`, `ambient` — each `"#RRGGBB"` or `"#RRGGBBAA"`. `apps/drydock-desktop/src/brand.rs` documents where each one is used. | Drydock's |

Anything left out is Drydock's. An unknown key, a malformed colour or an invalid name fails the
build with a message naming the key, so a typo never falls back silently. `cargo test` checks the
palette's text contrast against WCAG AA, so an unreadable palette fails too.

## Building

```bash
cargo build --release -p drydock-desktop
```

With a `brand/` folder that builds the product, without one it builds Drydock. The executable is
always `target/release/Drydock(.exe)` — the name the build system knows it by; the release workflow
publishes it under the product's name, and a product repository can add a small script that copies
it to `<name>.exe` for local use. To build another brand folder without moving it, point
`DRYDOCK_BRAND_DIR` at it (relative to the repository root):

```bash
DRYDOCK_BRAND_DIR=docs/brand-template cargo run -p drydock-desktop
```

Both embed the proxy address and signing secret the same way (`crates/drydock-core/*.secret`
locally, repository secrets in CI), so a local white-label build reaches the same proxy as a local
Drydock build. To look at every page without clicking through:
`cargo run -p drydock-desktop --features screenshot -- --screenshot <dir>`.

## Setting up a product repository

1. Create the repository and fill it with Drydock:
   ```bash
   git clone https://github.com/NhMarco/Drydock.git <product>
   cd <product>
   git remote rename origin upstream
   git remote add origin https://github.com/<owner>/<product>.git
   ```
2. Add `brand/` (start from `docs/brand-template/`), commit it, and push:
   `git push -u origin main`.
3. In the new repository, **Settings → Secrets and variables → Actions → Secrets**:
   `DRYDOCK_PROXY_BASE_URL` and `DRYDOCK_HMAC_SECRET` — the same values as Drydock's.
4. Release exactly as Drydock does: `git tag vX.Y.Z && git push origin vX.Y.Z`.

The release workflow reads the name from `brand/brand.toml`, names the assets `<name>-<platform>`,
and bakes *that* repository in as the update channel. So a product's installs update from its own
releases, Drydock's from Drydock's, and neither ever picks up the other's binary — the updater also
only accepts assets named after its own product. A local build of a white-label product has **no**
update channel, so it can never replace itself with somebody else's release.

## Keeping it current

Everything product-specific is in `brand/`, a folder Drydock never has, so an update never
conflicts:

```bash
git pull upstream main     # upstream = this Drydock repository
git push origin main
git tag vX.Y.Z && git push origin vX.Y.Z
```

Drydock's CI builds and tests once as Drydock and once with `docs/brand-template` (which leaves some
colours and features out), so a change here that would break white-label builds fails before it is
merged. The product repository's own CI runs the same checks against its real `brand/` folder.

## Rules that keep white-label builds cheap

These apply to changes in Drydock itself:

- **No colour literals in the UI.** A new shade goes into `Palette` — with Drydock's value in
  `DRYDOCK`, a key in `PALETTE_KEYS` in `apps/drydock-desktop/build.rs`, and a line in the table
  above — or products silently miss it.
- **No product name in UI text.** Use `branded!("… {product} …")` (or `branded!(upper "…")` for
  all-caps labels); in `const` tables write `{product}` and draw the text through `with_product`.
- **New optional features get a flag in `Features`** (and in `FEATURE_KEYS`), checked where the
  feature is offered and where its data is fetched — a product without the feature should never
  download what it cannot show.
- **Backend identifiers stay Drydock's.** Renaming a request header, an environment variable, the
  activation salt or its `.Drydock` marker per product would split products that must stay
  interchangeable. The data directory is the deliberate exception: it is each product's own.
- **New brand keys are additive.** A key a brand leaves out must fall back to Drydock's, so existing
  product repositories keep building after the pull without touching their `brand.toml`.
