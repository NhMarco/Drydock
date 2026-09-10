# Changelog

All notable Drydock changes are documented here. The project follows semantic versioning.

## 1.1.0 - 2026-09-11

Drydock now hosts the emulator binaries itself, and the game pages were cut down to a couple of
buttons.

### Emulator

- The loader proxy, `coldloader.dll` and the SteamStub loader are served from this repository's
  `emu/` folder instead of third-party release assets, so a DLL can be replaced by pushing to the
  repo and no longer depends on someone else's downloads staying put. gbe_fork is unchanged and still
  comes from its own release. Existing caches re-download once.
- `steam_settings\load_dlls\` receives the SteamStub loader matching the game's architecture in place
  of the two generic Steamworks stubs. **32-bit games get a loader there for the first time** — they
  previously got nothing.
- Cracking into a game folder no longer destroys anything: every file the crack would overwrite is
  moved to `<name>.bak` first. Re-cracking leaves an existing `.bak` alone, so the genuine original
  is never buried under a previous crack's file.

### Steam manifests

- Steam drops an app's depot manifests on an account switch, a cleared cache or an uninstall, and the
  unlock then quietly stops resolving. Drydock now notices and restores its own stored copies without
  touching the network. It checks when Steam starts or stops, otherwise every ten minutes, and a
  check is one `metadata` call per manifest — the manifests themselves are only read when something
  is actually missing.

### Interface

- The game page's Add/Cracked/Update/Remove buttons and the repack sources are one split button: the
  main action adds the latest unlock (or updates it once added), everything else sits behind the
  arrow. The arrow only appears when there is a real choice.
- Library rows are down to two controls — the prominent action (Play, Install or Set .exe) and a
  split button holding the rest. Re-linking a game's `.exe` is reachable again, which it was not once
  one had been picked.
- Settings has a games folder at the top, for where Drydock installs what it downloads. **This
  setting existed but did nothing**: downloads always went to Steam's `steamapps\common`. It is now
  honoured, with that path as the fallback when the field is empty. A game Steam already has
  installed is still updated where it is, so an update cannot start a second copy elsewhere.
- Launching a Drydock-downloaded game that has no crack in its folder now asks whether to crack it
  first, once per game per session, instead of letting it fail silently.

### Fixed

- Exported crack ZIPs could not be fully extracted with Windows Explorer: it failed on
  `steamclient64.dll` with an unspecified error while extracting every other file, leaving a crack
  folder that looked complete but was not. The deflate streams the ZIP writer produced were valid but
  something Explorer's extractor mishandles past roughly 13 MiB of compressible input; the compressor
  backend was swapped for one whose output it accepts. Archives written by earlier versions still
  open in 7-Zip or `Expand-Archive`.

## 1.0.1 - 2026-09-10

Hotfix for unlocks that were installed incomplete.

- **Unlock Lua files were being truncated.** The proxy stripped every single-argument
  `addappid(<id>)` line on the assumption that it was always a depot with no decryption key. It is
  also how an app declares ownership of itself and its DLC, so those lines were deleted too — one
  title went from 24 lines to 2, with the app's own ID gone. A bare line is now only dropped when the
  same file pins that ID to a manifest and supplies no key for it, which is the case the guard was
  written for. **Re-add or update any game added before this release**; the Lua already on disk is
  not repaired retroactively.
- Adding or updating a game now also copies its depot manifests into `Steam/depotcache`, so Steam
  installs the exact build the unlock pins instead of resolving one itself.
- Both files are additionally kept in Drydock's own store under the data directory, and restored
  before an install. Steam deletes an app's manifests when it is uninstalled, so this is what makes a
  reinstall work without fetching the depot package again.
- The library reads installed unlock Lua files from `config/stplug-in` directly, so games no longer
  disappear from it when the settings record of them is lost.

## 1.0.0 - 2026-09-10

First release under the Drydock name. The version resets to 1.0.0 with the new project and its own
release channel; entries below this one belong to the predecessor and are kept for history.

- Renamed the project throughout: binary, crates, data directory (`%LOCALAPPDATA%\Drydock`), request
  headers, environment variables and release channel.
- New visual identity: a palette built from brass, verdigris and rust on slate, replacing a scheme
  that had used Steam's own brand blue, plus a new application mark.
- **Configuration is now resolvable at runtime.** `DRYDOCK_PROXY_BASE_URL`, `DRYDOCK_HMAC_SECRET` and
  `DRYDOCK_UPDATE_REPOSITORY` resolve from environment → settings → build-time default, so a stock
  build can be pointed at a self-hosted proxy without rebuilding. Settings ▸ Proxy / Self-hosting
  exposes them, and `--config` prints what resolved and from where, with secrets redacted.
- The library now reads installed unlock Lua files from `config/stplug-in` directly instead of
  trusting only its own settings record, so games no longer disappear when that record is lost.
- Settings are loaded recoveringly: a corrupt or half-written `settings.json` falls back to its
  backup, and an unsalvageable file is quarantined instead of being silently replaced by defaults.
- Crash reports are written to `crash.log` in the data directory. Release builds have no console, so
  panics previously left no trace at all.
- Depot downloads check free space before writing, skip the resume verification for files they just
  created, and reject archive paths that could escape the install root.
- Caches are bounded: the image cache gained a stable key, a memory budget and an age sweep; the
  store cache is swept on startup; the proxy's file cache evicts expired entries.
- Proxy: per-provider credentials are only required when that provider is enabled, concurrent misses
  for the same file collapse into one upstream fetch, and the hourly gamelist refresh no longer
  round-trips ~100 MB of JSON through an extra parse.

## 2.1.1 - 2026-08-17

- Always render the app in its dark theme regardless of the operating system's light/dark setting, fixing white search fields, combo boxes, scrollbars, and the panel separator on machines set to a light system theme.

## 2.1.0 - 2026-08-17

- Install the Steam Service payload directly beside `steam.exe` (the Steam root) instead of into `config/stplug-in`; per-app Lua unlocks still live in `config/stplug-in`.
- Migrate an older `config/stplug-in` Service install to the new location automatically, leaving per-app Lua unlocks untouched.
- Disable the details-page Install button until the app's unlock Lua is present in the plug-in folder (i.e. the app has been added to Steam).
- Refresh the installed-game library and Steam Service status on every page switch, so the activatable list and per-app buttons stay current without restarting the app.

## 0.1.0 - Unreleased

- Created the native Rust workspace and portable core.
- Added Steam-library and manifest discovery for Windows, Linux, and macOS.
- Added verified per-game manifest update preferences and Steam restart support.
- Added the cinematic desktop shell, local search, Store details, lazy artwork, and language updates.
- Added machine/App-ID-bound encrypted activation request codes with a 30-minute lifetime.
- Added the complete request fallback used when the short-code service is unavailable.
- Added signed response verification, a verified-entitlement notice, and automatic manifest protection.
- Switched the game list to the Ryuu API (`/api/games`, gzip-compressed) and per-app unlock Lua downloads to `/api/download/{appid}?file_type=lua`, authenticated with a build-time-embedded API key.
- Cap the home catalog to a searchable page since the Ryuu list holds tens of thousands of games.
- Added the `NhMarco/MFB` payload client with Git-blob-SHA verification of every downloaded file (still used for the Steam Service payload).
- Added Steam Service status/install/update/repair and transactional install with staging, verification, and rollback.
- Added transactional per-app "Add to Steam"/"Remove from Steam" of the verified Lua unlock files.
- Exposed separate Steam stop/start controls so protected plug-in files are changed while Steam is closed.
- Added a SHA-256-verified updater for native Windows/Linux x64 and ARM64 releases.
- Derive official binary versions from validated release tags instead of a manually edited constant.
- Added a Windows release gate that waits for the GUI process and verifies its real exit code.
- Made Steam manifest scanning resilient to broken files and deterministic across duplicate libraries.
- Added running-process and Windows Registry Steam discovery plus Flatpak and Snap locations.
- Clamp startup size to the monitor and use a safe visible initial position.
- Keep every sidebar action reachable through vertical scrolling at the minimum window size.
- Load and cache the Steam user-review summary alongside each details page.
- Restart Steam through its graceful shutdown command with a verified 20-second timeout.
- Retry Steam metadata once with backoff when the Store returns HTTP 429.
- Fall back to visibly marked stale Store details when the network is unavailable.
- Lock CI dependencies and test every updater asset against both native workflow matrices.
- Refresh the validated supported-app catalog in the background while retaining an instant offline catalog.
- Persist refreshed catalogs transactionally with rollback on Windows rename failures.
- Remove unused file, SVG, and animation-specific GUI loaders from release builds.
- Limit remote catalog checks to once per 30 minutes across application restarts.
- Limit automatic release checks to once per 15 minutes while keeping manual checks immediate.
- Offer the correct Steam install or play action from game details, even when Store data is offline.
