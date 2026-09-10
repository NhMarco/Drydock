# Feature-parity contract (historical)


> Kept for history: this tracked the original Rust rewrite against the .NET application it replaced.
> Every item is complete; it is not a roadmap.

This file tracks the rebuild against the behaviour of the predecessor .NET application. A checked item
must be backed by a test or a manual runtime verification; visual placeholders do not count.

## Shell and navigation

- [x] Responsive Home, Activation, Updates, and Settings navigation
- [x] Adaptive initial window size and minimum usable size
- [x] Version and "Developed with love" footer
- [x] Guided "How it works" dialog
- [x] Non-blocking background actions with a busy overlay and surfaced errors

## Catalog and details

- [x] Load the complete allowed app catalog with database-derived names on first launch
- [x] Refresh and cache a validated remote catalog without delaying first-launch search
- [x] Search locally known apps by name or App ID without scrolling
- [x] Rate-limited Steam Store metadata and persistent caches
- [x] Lazy image loading for visible rows
- [x] Details page with information, requirements, description, and screenshot carousel

## Steam integration

- [x] Discover common Steam locations on Windows, Linux, and macOS
- [x] Discover additional libraries from `libraryfolders.vdf`
- [x] Parse installed `appmanifest_*.acf` files
- [x] Limit activation choices to supported apps that Steam actually installed
- [x] Platform-specific Steam stop/start/restart adapter
- [x] Steam service status, install, repair, and reinstall where supported
- [x] Add/remove supported app integration and transactional verification
- [x] Detect known conflicting software through processes, installed programs, and Steam artifacts

## Updates and settings

- [x] Read and write the existing portable `settings/settings.json` shape
- [x] Auto-detect or select the Steam root with a native folder dialog
- [x] Show installed games and persisted per-game update preferences
- [x] Apply, verify, and reapply platform-appropriate manifest protection
- [x] Request administrator access at Windows startup before protected manifest changes
- [x] SHA-256-verified GitHub Release self-update for every released target
- [x] Reuse compatible Drydock settings and device identity when installed in the same portable folder

## Activation and language

- [x] Portable device identity and machine/App-ID-bound entitlement request
- [x] Short request upload with 30-minute expiry and strict host/filename validation
- [x] Complete request fallback when the short-code upload service is unavailable
- [x] Signed entitlement verification with tampered/wrong-machine/wrong-App-ID tests
- [x] Transactional installation of explicitly authorized application files
- [x] Verified-entitlement success dialog without claiming unperformed file installation
- [x] Supported-language discovery and atomic `configs.user.ini` update in the portable core

## Release validation

- [x] Windows x64 release build
- [x] Headless packaged-binary self-test wired into every native CI/release target
- [ ] Windows ARM64 build validation
- [ ] Linux x64 build and smoke test
- [ ] Linux ARM64 build validation
- [x] Clean portable-state startup self-test
- [x] Isolated Windows x64 executable replacement smoke test
- [ ] Update-from-previous-release validation on every target
