# Changelog

All notable Drydock changes are documented here. The project follows semantic versioning.

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
