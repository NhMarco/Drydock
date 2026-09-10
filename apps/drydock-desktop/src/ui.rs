use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use drydock_core::{
    APP_VERSION, ActivationRequestService, AddedAppState, AppCatalog, AppUpdater, CatalogApp, CdnClient,
    CloudProvider, CloudRedirect, CloudSettings, ConfigSource, ConflictingSoftwareStatus, DenuvoWatchClient,
    DepotData, DownloadProgress, DownloadedDll, EmuTemplateInput, FixEntry, FixStatus, GameLanguageOptions,
    LoadOutcome, OpenSteamTool, PeArch, PortablePaths, PreparedUpdate, ProxyClient, QueueEffect,
    QueuedDownload, RepackApp, S3Credentials, Settings, SteamDiscovery, SteamManifest, SteamServiceState,
    SteamServiceStatus, SteamStoreClient, SteamStoreDetails, SteamUriAction, StoreCapsule, StoreFeatured,
    VerifiedEntitlement, achievement_image_urls, add_app_files, apply_denuvo_fix, apply_language,
    clear_previous_token_files, cloud, depot, detect_conflicting_software, detect_pe_arch, discover_steam,
    download_queue, ensure_toolchain, fetch_achievement_images, fetch_install_dir, fetch_reframework_dll,
    fetch_windows_arch, fetch_windows_executables, fix_status, install_magicfiles, install_service,
    is_valid_steam_directory, load_cached_denuvo_appids, load_catalog_apps, load_dll_files, load_manifests,
    open_link, open_steam_uri, overlay_sound_bytes, read_denuvo_appids, read_language_options,
    remove_app_files, remove_paths, resolve_game_root, restart_steam, run_and_capture_token_request,
    save_catalog_apps, save_denuvo_appids, scan_crack_files, service_status, set_manifest_updates_enabled,
    start_steam, stop_steam, toolchain_dlls, uninstall_service, updates_enabled,
};
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

// Drydock palette — oxidised metals in a shipyard, not a storefront.
//
// The previous scheme was Steam's own: navy grounds with `#66C0F4`, which is literally Valve's brand
// blue. It made the app read as a Steam product. This one is built from what a dry dock is made of —
// brass fittings, copper gone to verdigris, iron gone to rust — over a cold slate ground. The
// neutrals carry a faint warm bias so they sit under the brass instead of fighting it, and the three
// semantic colours (verdigris / signal / rust) are separated by hue *and* lightness so state stays
// readable without relying on colour alone.
const BACKGROUND: Color32 = Color32::from_rgb(21, 24, 27); // #15181B cold slate
const SURFACE: Color32 = Color32::from_rgb(30, 35, 40); // #1E2328
const SURFACE_RAISED: Color32 = Color32::from_rgb(40, 47, 54); // #282F36
const BORDER: Color32 = Color32::from_rgb(56, 66, 76); // #38424C
const TEXT: Color32 = Color32::from_rgb(232, 230, 227); // #E8E6E3 warm off-white
const MUTED: Color32 = Color32::from_rgb(142, 146, 153); // #8E9299
/// Brand accent: brass fittings. Primary buttons, active nav, focus.
const ACCENT: Color32 = Color32::from_rgb(223, 160, 74); // #DFA04A
/// Lighter brass for secondary emphasis, spinners and inline highlights.
const ACCENT_SOFT: Color32 = Color32::from_rgb(237, 190, 122); // #EDBE7A
/// Darkened brass for badges and the gradient's far stop.
const ACCENT_DEEP: Color32 = Color32::from_rgb(184, 127, 51); // #B87F33
/// Oxidised copper — "installed", "ready", "play".
const VERDIGRIS: Color32 = Color32::from_rgb(79, 168, 139); // #4FA88B
/// Signal yellow for warnings. Lighter and more saturated than the brass accent so the two never
/// read as the same thing, and it is always paired with a ⚠ glyph rather than carrying meaning alone.
const AMBER: Color32 = Color32::from_rgb(242, 201, 76); // #F2C94C
/// Rusted iron for destructive actions and errors.
const DANGER: Color32 = Color32::from_rgb(199, 92, 92); // #C75C5C
const SIDEBAR_FILL: Color32 = Color32::from_rgb(17, 20, 23); // #111417
const CATALOG_REFRESH_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
/// Steam header art is 460×215 — this ratio is used for card cover heights and detail artwork.
const STEAM_HEADER_ASPECT: f32 = 0.467;
/// Height of the top navigation bar and the bottom status strip that frame the storefront.
const TOP_NAV_HEIGHT: f32 = 52.0;
const STATUS_BAR_HEIGHT: f32 = 30.0;
const MIN_CONTENT_GUTTER: f32 = 24.0;
/// Every tab's content is capped to this single width and centred, so all pages share one aligned
/// column and none stretches edge-to-edge on wide monitors; on narrower windows it fills to within
/// `MIN_CONTENT_GUTTER` of each side. It equals the storefront hero image's width, so the Store
/// banner fills edge to edge with no letterbox bars. `STORE_HERO_ASPECT` is Steam's `library_hero.jpg`
/// ratio (1920×620), used to size that banner.
const CONTENT_WIDTH: f32 = 1040.0;
const STORE_HERO_ASPECT: f32 = 3.1;

/// Cached result of validating the Steam directory draft text field.
#[derive(Clone, Debug, PartialEq)]
enum SteamDirValidation {
    /// The field is empty — no indicator shown.
    Empty,
    /// The path does not exist on disk.
    NotFound,
    /// The path exists but is missing the platform Steam launcher.
    MissingExecutable,
    /// The path has a launcher but is missing the `steamapps` folder.
    MissingSteamapps,
    /// The path looks like a valid Steam root.
    Valid,
}

/// Simple rolling-window rate limiter that keeps a single user from spamming the API.
struct RateLimiter {
    window: Duration,
    max: usize,
    hits: Vec<Instant>,
}

impl RateLimiter {
    fn new(max: usize, window: Duration) -> Self {
        Self {
            window,
            max,
            hits: Vec::new(),
        }
    }

    /// Records a request. Returns `Ok` if within the limit, or `Err(retry_after)` if not.
    fn check(&mut self) -> Result<(), Duration> {
        let now = Instant::now();
        self.hits.retain(|hit| now.duration_since(*hit) < self.window);
        if self.hits.len() >= self.max {
            let oldest = self.hits.first().copied().unwrap_or(now);
            return Err(self.window.saturating_sub(now.duration_since(oldest)));
        }
        self.hits.push(now);
        Ok(())
    }
}
const UPDATE_CHECK_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// Shortest gap between network-backed Steam Service status checks triggered by page
/// switches, so rapidly flipping tabs does not hammer the payload repository.
const SERVICE_RECHECK_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Home,
    Library,
    Details,
    Activation,
    Tools,
    Cloud,
    Updates,
    Settings,
    Guide,
    Downloads,
}

/// The Store's sub-tabs, mirroring Steam's own storefront navigation. `DenuvoWatch` is Drydock-only:
/// the set of games that actually need activation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum StoreTab {
    #[default]
    Featured,
    NewReleases,
    Repacks,
    DenuvoWatch,
}

impl StoreTab {
    const ALL: [(Self, &'static str); 4] = [
        (Self::Featured, "Featured"),
        (Self::NewReleases, "New Releases"),
        (Self::Repacks, "Repacks"),
        (Self::DenuvoWatch, "Denuvo"),
    ];
}

/// Which walkthrough the How It Works page is showing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuideFlow {
    Activation,
    Fixes,
}

/// Which store's activation the Activation page is currently showing (switched in-place, not a
/// separate page). Steam is the full flow; Ubisoft is the magicfiles/token.ini flow; EA is planned.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ActivationProvider {
    #[default]
    Steam,
    Ubisoft,
    Ea,
}

/// Home-search filter that restricts results to apps with a repack, optionally from one repacker.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum RepackFilter {
    #[default]
    Any,
    /// Any app that has at least one repack source.
    AnyRepack,
    /// Only apps with a repack from this exact repacker (e.g. "DODI", "FitGirl").
    Repacker(String),
}

impl RepackFilter {
    /// Short label for the filter dropdown's button.
    fn label(&self) -> String {
        match self {
            Self::Any => "Any".to_owned(),
            Self::AnyRepack => "All repacks".to_owned(),
            Self::Repacker(name) => name.clone(),
        }
    }
}

/// Home-search filter that restricts results to apps with a fix of the given kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FixFilter {
    #[default]
    Any,
    Denuvo,
}

impl FixFilter {
    fn label(self) -> &'static str {
        match self {
            Self::Any => "Any",
            Self::Denuvo => "Denuvo",
        }
    }
}

pub struct DrydockApp {
    page: Page,
    /// The page shown on the previous frame, used to run a state refresh on every page switch.
    last_page: Page,
    /// When the Steam Service status was last re-checked from a page switch (throttled).
    last_service_check: Option<Instant>,
    guide_flow: GuideFlow,
    paths: PortablePaths,
    settings: Settings,
    /// Set when the settings file on disk could not be parsed or read at startup. `settings` then
    /// holds defaults, so every save is blocked until the user explicitly discards the broken file —
    /// otherwise the first write would replace their real library with an empty one.
    settings_read_only: bool,
    steam: SteamDiscovery,
    conflicts: ConflictingSoftwareStatus,
    manifests: Vec<SteamManifest>,
    catalog: Vec<CatalogApp>,
    /// App IDs present in the catalog (proxy gamelist), for O(1) "is this game available?" checks —
    /// used to filter the storefront shelves down to games Drydock can actually get.
    catalog_ids: std::collections::HashSet<u32>,
    /// On-demand resolver for catalog rows' real header art (the App-ID CDN guesses 404 for
    /// hashed-CDN titles). Shared by the Denuvo tab and the nav-search results.
    header_resolver: HeaderResolver,
    catalog_receiver: Option<Receiver<Result<Vec<CatalogApp>, String>>>,
    available_tags: Vec<String>,
    selected_tag: Option<String>,
    // Games that actively use Denuvo, from the "Denuvo Watch" Steam curator — the complete,
    // always-current activation-needed set, fetched once and cached, so the filter is instant.
    denuvo_appids: std::collections::HashSet<u32>,
    denuvo_loaded: bool,
    denuvo_receiver: Option<Receiver<Result<Vec<u32>, String>>>,
    // Per-app game fixes available in the GitHub `Files/fix` folder (Lua + zip).
    fixes: Vec<FixEntry>,
    fixes_loaded: bool,
    fixes_receiver: Option<Receiver<Result<Vec<FixEntry>, String>>>,
    // Per-app repacks (external download links) served by the proxy from `Files/repacks.json`.
    repacks: Vec<RepackApp>,
    repacks_loaded: bool,
    repacks_receiver: Option<Receiver<Result<Vec<RepackApp>, String>>>,
    // Home-search filters, plus the lookup structures they need (rebuilt when repacks/fixes load so
    // filtering the full catalog stays O(1) per app instead of scanning the repack/fix lists).
    repack_filter: RepackFilter,
    fix_filter: FixFilter,
    /// Distinct repacker names across all repacks, sorted, for the Repacks filter dropdown.
    available_repackers: Vec<String>,
    /// app_id -> lowercased repacker names for that app, for repack-filter matching.
    repackers_by_app: std::collections::HashMap<u32, Vec<String>>,
    /// The set of apps that have a Denuvo fix available, for fix-filter matching.
    fix_flags_by_app: std::collections::HashSet<u32>,
    // App whose Steam updates should be blocked once the running Apply Fix succeeds.
    pending_fix_block: Option<u32>,
    catalog_limiter: RateLimiter,
    download_limiter: RateLimiter,
    search: String,
    steam_directory_draft: String,
    /// Cache of the last validated draft path and its result, to avoid re-checking every frame.
    validated_steam_path: Option<(String, SteamDirValidation)>,
    selected_app: Option<u32>,
    /// The game highlighted in the Steam-style Library rail (right-hand overview shows this one).
    library_selected: Option<u32>,
    /// The folder chosen for the "Add game to Drydock" flow; `Some` puts the Library into add-mode
    /// (pick which game the folder is), cleared when the game is added or the flow is cancelled.
    add_game_folder: Option<PathBuf>,
    /// The game-picker query for the "Add game to Drydock" flow.
    add_game_search: String,
    /// Background folder/exe detection for the "Add game to Drydock" flow.
    add_game_receiver: Option<Receiver<Result<AddGameOutcome, String>>>,
    /// Background detection that registers a just-finished depot download in the Drydock library.
    download_install_receiver: Option<Receiver<Result<AddGameOutcome, String>>>,
    language_options: Option<GameLanguageOptions>,
    language_directory: Option<PathBuf>,
    language_selection: String,
    /// The folder chosen in the Tools tab's language changer.
    tools_language_path: String,
    /// The App ID typed into the Tools emulator cracker.
    emu_appid: String,
    /// Deploy the loader proxy as `winmm.dll` instead of `version.dll`.
    emu_loader_winmm: bool,
    /// The emulator-cracker architecture choice (auto-detect, or forced x64/x86).
    emu_arch: EmuArch,
    /// Also bundle praydog's latest REFramework nightly (`dinput8.dll`) next to the exe (opt-in).
    emu_reframework: bool,
    /// Background job for the local emulator cracker (App ID → depots → files).
    emu_receiver: Option<Receiver<Result<String, String>>>,
    status: String,
    status_error: bool,
    background_action: Option<Receiver<Result<String, String>>>,
    busy_label: Option<String>,
    details_app_id: Option<u32>,
    store_details: Option<SteamStoreDetails>,
    store_receiver: Option<Receiver<(u32, Result<SteamStoreDetails, String>)>>,
    store_loading: bool,
    // Store tab (Steam-style storefront): the selected sub-tab and the live featured feed.
    store_tab: StoreTab,
    featured: Option<StoreFeatured>,
    featured_loading: bool,
    featured_error: Option<String>,
    featured_receiver: Option<Receiver<Result<StoreFeatured, String>>>,
    screenshot_index: usize,
    activation_request_code: String,
    activation_receiver: Option<Receiver<Result<String, String>>>,
    activation_verify_receiver: Option<Receiver<Result<VerifiedEntitlement, String>>>,
    verified_entitlement: Option<VerifiedEntitlement>,
    entitlement_success_app: Option<String>,
    response_code: [String; 8],
    // Foreign-install activation: dynamic game search, chosen/verified game folder, and the
    // background checks that resolve the install root and screen for crack/HV artifacts.
    activation_search: String,
    activation_path: String,
    activation_root: Option<PathBuf>,
    activation_check_app: Option<u32>,
    activation_check_receiver: Option<Receiver<Result<ActivationCheck, String>>>,
    activation_remove_receiver: Option<Receiver<Result<(u32, PathBuf), String>>>,
    pending_crack: Option<PendingCrack>,
    // Which store's activation is on screen, and the Ubisoft flow's state: the background
    // magicfiles+launch+capture step, the resulting activation code, and where token.ini installs.
    activation_provider: ActivationProvider,
    ubisoft_prepare_receiver: Option<Receiver<Result<UbisoftPrepared, String>>>,
    ubisoft_activation_code: String,
    ubisoft_exe_dir: Option<PathBuf>,
    update_receiver: Option<Receiver<Result<Option<PreparedUpdate>, String>>>,
    exit_for_update: bool,
    // Cloud tab (CloudRedirect): provider form + the background DLL download / OAuth sign-in.
    cloud: CloudForm,
    cloud_download_receiver: Option<Receiver<Result<DownloadedDll, String>>>,
    cloud_oauth_receiver: Option<Receiver<Result<CloudSettings, String>>>,
    /// Cached CloudRedirect DLL state — `dll_status` hashes the file, so it is refreshed on page
    /// switches and after cloud actions rather than read from the render loop. `None` = no Steam root.
    cloud_dll_status: Option<drydock_core::DllStatus>,
    /// Cached provider from the CloudRedirect `config.json`, refreshed alongside the DLL state.
    cloud_provider: Option<CloudProvider>,
    service_status: Option<SteamServiceStatus>,
    service_receiver: Option<Receiver<Result<ServiceOutcome, String>>>,
    // Native depot download: the active download/verify job (background thread → progress channel),
    // shown in the bottom Downloads bar. The pending queue lives in `settings.download_queue` (front
    // = current), so it persists across launches and auto-resumes.
    download_job: Option<DownloadJob>,
    /// The current download is paused (by the user, or stopped by an error) — its queue entry stays
    /// at the front so it can resume, but no thread is running.
    download_paused: bool,
    /// When a download stopped because of an error (not a user pause), the message to show.
    download_error: Option<String>,
    /// The most recent progress tick, kept so a paused download still shows its position in the bar.
    download_last: Option<DownloadProgress>,
    /// Set when the running thread is being cancelled only to immediately start a reordered front
    /// (activate a queued download, or send the active one back into the queue) — not a pause.
    download_switch_pending: bool,
    started: Instant,
}

/// A running (or just-finished) depot download or verify, driven by a background thread.
struct DownloadJob {
    app_id: u32,
    name: String,
    kind: DownloadKind,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<DownloadUpdate>,
    progress: Option<DownloadProgress>,
    /// Smoothed download speed in bytes/sec, its running peak, plus the last (time, done_bytes)
    /// sample the estimate came from.
    speed_bps: f64,
    peak_bps: f64,
    sample: Option<(Instant, u64)>,
    /// `Some` once the job ended: `Ok(summary)` or `Err(message)`.
    finished: Option<Result<String, String>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DownloadKind {
    Download,
    Verify,
}

enum DownloadUpdate {
    Progress(DownloadProgress),
    Finished(Result<String, String>),
}

/// Outcome of the pre-activation folder check: the verified install root, and whether crack/HV
/// artifacts must be removed before a request code is generated.
enum ActivationCheck {
    Ready { root: PathBuf },
    NeedsRemoval { root: PathBuf, files: Vec<PathBuf> },
}

/// Result of the Ubisoft "prepare" step: the activation code to paste into a ticket, and the exe
/// directory the response token's `token.ini` will be installed into.
struct UbisoftPrepared {
    activation_code: String,
    exe_dir: PathBuf,
}

/// State for the "crack files found" prompt: the app and verified root, and the paths to delete.
struct PendingCrack {
    app_id: u32,
    root: PathBuf,
    files: Vec<PathBuf>,
}

/// Cloud-tab form state: the selected CloudRedirect provider and its path/credential fields.
struct CloudForm {
    provider: CloudProvider,
    folder_path: String,
    local_path: String,
    account_id: String,
    endpoint: String,
    region: String,
    access_key_id: String,
    secret_access_key: String,
    bucket: String,
    key_prefix: String,
}

impl Default for CloudForm {
    fn default() -> Self {
        Self {
            provider: CloudProvider::Folder,
            folder_path: String::new(),
            local_path: String::new(),
            account_id: String::new(),
            endpoint: String::new(),
            region: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            bucket: String::new(),
            key_prefix: String::new(),
        }
    }
}

impl CloudForm {
    /// The file-based settings to persist, or `None` for OAuth providers (those go through the
    /// browser sign-in, which writes the token file and settings itself).
    fn to_settings(&self) -> Option<CloudSettings> {
        let credentials = S3Credentials {
            account_id: self.account_id.trim().to_owned(),
            endpoint: self.endpoint.trim().to_owned(),
            region: self.region.trim().to_owned(),
            access_key_id: self.access_key_id.trim().to_owned(),
            secret_access_key: self.secret_access_key.clone(),
            bucket: self.bucket.trim().to_owned(),
            key_prefix: self.key_prefix.trim().to_owned(),
        };
        match self.provider {
            CloudProvider::LocalOnly => Some(CloudSettings::LocalOnly {
                path: self.local_path.trim().to_owned(),
            }),
            CloudProvider::Folder => Some(CloudSettings::Folder {
                path: self.folder_path.trim().to_owned(),
            }),
            CloudProvider::R2 => Some(CloudSettings::R2(credentials)),
            CloudProvider::S3 => Some(CloudSettings::S3(credentials)),
            CloudProvider::GoogleDrive | CloudProvider::OneDrive => None,
        }
    }
}

/// Result of a Steam Service background operation, applied on the UI thread.
enum ServiceOutcome {
    /// A status refresh or a completed (re)install; carries the new state.
    Status {
        status: SteamServiceStatus,
        note: String,
    },
    /// An app's Lua unlock files were installed; persist them in settings.
    Added {
        app_id: u32,
        files: Vec<String>,
        note: String,
    },
    /// An app's Lua unlock files were removed; drop it from settings.
    Removed { app_id: u32, note: String },
}

impl DrydockApp {
    pub fn new(context: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&context.egui_ctx);
        install_style(&context.egui_ctx);

        let (paths, path_warning) = match PortablePaths::discover() {
            Ok(paths) => (paths, None),
            Err(error) => (
                PortablePaths::at("."),
                Some(format!(
                    "Portable paths could not be discovered ({error}); falling back to the current directory."
                )),
            ),
        };
        // Reclaim long-untouched cache files before anything repopulates them. Both caches grow one
        // file per app/artwork the user ever looked at and had no eviction at all, so on a
        // long-running install they only ever got bigger. Off the render path, on a detached thread:
        // it is pure disk housekeeping and nothing waits for it.
        {
            let cache_dir = paths.cache_dir();
            std::thread::spawn(move || {
                crate::image_cache::DiskImageCache::sweep(&cache_dir);
                SteamStoreClient::sweep_stale_cache(&cache_dir.join("store-details"));
            });
        }
        // A persistent disk cache for the storefront/library CDN images, registered before egui's
        // default HTTP loader so it takes priority (loaders run in registration order). Cached art is
        // then served instantly from disk on later frames and launches instead of re-downloading.
        context
            .egui_ctx
            .add_bytes_loader(std::sync::Arc::new(crate::image_cache::DiskImageCache::new(
                &paths.cache_dir(),
            )));
        egui_extras::install_image_loaders(&context.egui_ctx);
        // Recovering load: a corrupt/half-written settings file falls back to the `.bak` copy, and a
        // file that cannot be salvaged is moved aside rather than silently replaced by defaults —
        // otherwise the next save would wipe `added_apps` / `installed_games` / `launch_paths`.
        let (mut settings, load_outcome) = Settings::load_recovering(&paths.settings_file());
        let settings_read_only = !load_outcome.save_is_safe();
        // Push the self-hosting overrides into the config layer before anything builds a
        // `ProxyClient`, so a user-supplied proxy address is in effect from the very first request.
        settings.apply_config_overrides();
        let (mut status, mut status_error) = match &load_outcome {
            LoadOutcome::Loaded => (String::from("Ready"), false),
            LoadOutcome::RecoveredFromBackup { reason } => {
                (format!("Settings recovered from the backup: {reason}"), true)
            }
            LoadOutcome::Quarantined { reason, quarantined } => (
                format!(
                    "Settings were unreadable ({reason}). The file was kept at {} and Drydock started with \
                     defaults — saving is disabled so nothing is overwritten. Use Settings ▸ DISCARD \
                     BROKEN SETTINGS once you have checked it.",
                    quarantined.display()
                ),
                true,
            ),
            LoadOutcome::Unreadable { reason } => (
                format!(
                    "Settings could not be read ({reason}). Saving is disabled so your library is not \
                     overwritten — close Drydock, fix the file, and start again."
                ),
                true,
            ),
        };
        if let Some(warning) = path_warning {
            status = warning;
            status_error = true;
        }
        let configured = path_if_present(&settings.steam_directory);
        let steam = discover_steam(configured);
        let manifests = match load_manifests(&steam) {
            Ok(manifests) => manifests,
            Err(error) => {
                status = format!("Steam library could not be read: {error}");
                status_error = true;
                Vec::new()
            }
        };
        for manifest in &manifests {
            if let Some(enabled) = settings.steam_updates_enabled.get(&manifest.app_id).copied() {
                if let Err(error) = set_manifest_updates_enabled(&manifest.manifest_path, enabled) {
                    status = format!("A saved game update policy could not be applied: {error}");
                    status_error = true;
                }
            } else if let Ok(enabled) = updates_enabled(&manifest.manifest_path) {
                settings.steam_updates_enabled.insert(manifest.app_id, enabled);
            }
        }
        let steam_directory_draft = steam.root.as_ref().map_or_else(
            || settings.steam_directory.clone(),
            |path| path.display().to_string(),
        );
        let conflicts = detect_conflicting_software(steam.root.as_deref());
        // Prefer the cached Ryuu game list; otherwise show the small embedded catalog instantly
        // while the full list is fetched in the background.
        let catalog = load_catalog_apps(&paths.cache_dir().join("games.json"))
            .ok()
            .filter(|apps| !apps.is_empty())
            .or_else(|| AppCatalog::embedded().ok().map(|catalog| catalog.apps))
            .unwrap_or_else(|| {
                manifests
                    .iter()
                    .map(|manifest| CatalogApp {
                        app_id: manifest.app_id,
                        name: manifest.name.clone(),
                        drm: false,
                        tags: Vec::new(),
                    })
                    .collect()
            });

        let header_resolver =
            HeaderResolver::new(paths.cache_dir().join("store-details"), context.egui_ctx.clone());
        let mut app = Self {
            page: Page::Home,
            last_page: Page::Home,
            last_service_check: None,
            guide_flow: GuideFlow::Activation,
            paths,
            settings,
            settings_read_only,
            steam,
            conflicts,
            manifests,
            catalog,
            catalog_ids: std::collections::HashSet::new(),
            header_resolver,
            catalog_receiver: None,
            available_tags: Vec::new(),
            selected_tag: None,
            denuvo_appids: std::collections::HashSet::new(),
            denuvo_loaded: false,
            denuvo_receiver: None,
            fixes: Vec::new(),
            fixes_loaded: false,
            fixes_receiver: None,
            repacks: Vec::new(),
            repacks_loaded: false,
            repacks_receiver: None,
            repack_filter: RepackFilter::Any,
            fix_filter: FixFilter::Any,
            available_repackers: Vec::new(),
            repackers_by_app: std::collections::HashMap::new(),
            fix_flags_by_app: std::collections::HashSet::new(),
            pending_fix_block: None,
            // At most one full game-list refresh per 5 minutes, and one unlock download per
            // minute, so a single user cannot hammer the Ryuu API.
            catalog_limiter: RateLimiter::new(1, Duration::from_secs(5 * 60)),
            download_limiter: RateLimiter::new(1, Duration::from_secs(60)),
            search: String::new(),
            steam_directory_draft,
            validated_steam_path: None,
            selected_app: None,
            library_selected: None,
            add_game_folder: None,
            add_game_search: String::new(),
            add_game_receiver: None,
            download_install_receiver: None,
            language_options: None,
            language_directory: None,
            language_selection: String::new(),
            tools_language_path: String::new(),
            emu_appid: String::new(),
            emu_loader_winmm: false,
            emu_arch: EmuArch::Auto,
            emu_reframework: false,
            emu_receiver: None,
            status,
            status_error,
            background_action: None,
            busy_label: None,
            details_app_id: None,
            store_details: None,
            store_receiver: None,
            store_loading: false,
            store_tab: StoreTab::default(),
            featured: None,
            featured_loading: false,
            featured_error: None,
            featured_receiver: None,
            screenshot_index: 0,
            activation_request_code: String::new(),
            activation_search: String::new(),
            activation_path: String::new(),
            activation_root: None,
            activation_check_app: None,
            activation_check_receiver: None,
            activation_remove_receiver: None,
            pending_crack: None,
            activation_provider: ActivationProvider::default(),
            ubisoft_prepare_receiver: None,
            ubisoft_activation_code: String::new(),
            ubisoft_exe_dir: None,
            activation_receiver: None,
            activation_verify_receiver: None,
            verified_entitlement: None,
            entitlement_success_app: None,
            response_code: std::array::from_fn(|_| String::new()),
            update_receiver: None,
            exit_for_update: false,
            cloud: CloudForm::default(),
            cloud_download_receiver: None,
            cloud_oauth_receiver: None,
            cloud_dll_status: None,
            cloud_provider: None,
            service_status: None,
            service_receiver: None,
            download_job: None,
            download_paused: false,
            download_error: None,
            download_last: None,
            download_switch_pending: false,
            started: Instant::now(),
        };
        if app.settings.auto_update_drydock && AppUpdater::can_self_update() {
            app.start_update_check(true);
        }
        app.recompute_tags();
        app.start_catalog_refresh(false);
        app.start_denuvo_refresh(false);
        app.start_fixes_refresh();
        app.start_repacks_refresh();
        app.refresh_service_status();
        // Resume an unfinished download from a previous session (the depot engine continues from the
        // chunks already on disk).
        app.start_front_download();
        app
    }

    /// Recomputes the genre filter options from the current catalog, keeping only tags that
    /// appear often enough to be useful and dropping a stale selection.
    fn recompute_tags(&mut self) {
        self.catalog_ids = self.catalog.iter().map(|app| app.app_id).collect();
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for app in &self.catalog {
            for tag in &app.tags {
                *counts.entry(tag.as_str()).or_default() += 1;
            }
        }
        let mut tags: Vec<(String, usize)> = counts
            .into_iter()
            .filter(|(_, count)| *count >= 20)
            .map(|(tag, count)| (tag.to_owned(), count))
            .collect();
        tags.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        self.available_tags = tags.into_iter().take(40).map(|(tag, _)| tag).collect();
        if let Some(selected) = &self.selected_tag
            && !self.available_tags.iter().any(|tag| tag == selected)
        {
            self.selected_tag = None;
        }
    }

    fn start_catalog_refresh(&mut self, force: bool) {
        if self.catalog_receiver.is_some() {
            return;
        }
        // The list is kept for 24 hours; only a forced refresh bypasses that window. The
        // schema suffix invalidates caches written before the drm/tags fields were added.
        let marker = self.paths.cache_dir().join("catalog-refresh.marker");
        let marker_value = format!("{APP_VERSION}:catalog-v2");
        if !force && refresh_marker_is_current(&marker, &marker_value, CATALOG_REFRESH_COOLDOWN) {
            return;
        }
        if let Some(parent) = marker.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&marker, marker_value);
        let (sender, receiver) = mpsc::channel();
        self.catalog_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = ProxyClient::new()
                .and_then(|client| client.fetch_catalog())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    /// Forces a game-list refresh from the Settings button, subject to the rate limiter.
    fn force_catalog_refresh(&mut self) {
        if self.catalog_receiver.is_some() {
            self.status = "A game-list refresh is already running.".into();
            self.status_error = false;
            return;
        }
        match self.catalog_limiter.check() {
            Ok(()) => {
                self.start_catalog_refresh(true);
                self.denuvo_loaded = false;
                self.start_denuvo_refresh(true);
                self.status = "Refreshing the game list…".into();
                self.status_error = false;
            }
            Err(retry) => {
                self.status = format!(
                    "Too many refreshes. Please wait {}s before trying again.",
                    retry.as_secs().max(1)
                );
                self.status_error = true;
            }
        }
    }

    fn poll_catalog_refresh(&mut self) {
        let Some(receiver) = self.catalog_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.catalog_receiver = None;
                if let Ok(apps) = result
                    && !apps.is_empty()
                {
                    let changed = apps != self.catalog;
                    self.catalog = apps;
                    self.recompute_tags();
                    if let Err(error) =
                        save_catalog_apps(&self.paths.cache_dir().join("games.json"), &self.catalog)
                    {
                        self.status = format!("The refreshed game list could not be cached: {error}");
                        self.status_error = true;
                    } else if changed {
                        self.status = format!("Game list updated: {} games", self.catalog.len());
                        self.status_error = false;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.catalog_receiver = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Whether an app needs activation, i.e. actively uses Denuvo (Steam "Denuvo Watch" curator).
    /// `None` only while the list is still loading; once loaded the answer is complete for every
    /// app, so the filter needs no per-card probing.
    fn needs_activation(&self, app_id: u32) -> Option<bool> {
        self.denuvo_loaded.then(|| self.denuvo_appids.contains(&app_id))
    }

    /// Fetches the "Denuvo Watch" curator list in the background (cached for a day). Falls back to
    /// any cached list if the network fetch fails, so the filter still works offline.
    fn start_denuvo_refresh(&mut self, force: bool) {
        if self.denuvo_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.denuvo_receiver = Some(receiver);
        let cache_path = self.paths.cache_dir().join("denuvo-appids.json");
        std::thread::spawn(move || {
            let result = fetch_denuvo_appids(&cache_path, force);
            let _ = sender.send(result);
        });
    }

    fn poll_denuvo_refresh(&mut self) {
        let Some(receiver) = self.denuvo_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.denuvo_receiver = None;
                match result {
                    Ok(appids) => {
                        self.denuvo_appids = appids.into_iter().collect();
                        self.denuvo_loaded = true;
                    }
                    Err(error) => {
                        // Mark as loaded so the filter is usable (as an empty set) rather than
                        // stuck on "loading"; surface the reason in the status line.
                        self.denuvo_loaded = true;
                        self.status = format!("Denuvo list unavailable: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.denuvo_receiver = None,
        }
    }

    /// Loads the GitHub "Denuvo" fixes in the background (the DepotBox "online fix" API was removed —
    /// Drydock now only offers the build-locked Denuvo fixes from the MFB repo).
    fn start_fixes_refresh(&mut self) {
        if self.fixes_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.fixes_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<FixEntry>, String> {
                let client = ProxyClient::new().map_err(|error| error.to_string())?;
                let mut fixes: Vec<FixEntry> = client
                    .denuvo_fixes()
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|(app_id, denuvo_fix)| FixEntry {
                        app_id,
                        name: String::new(),
                        denuvo: Some(denuvo_fix),
                    })
                    .collect();
                fixes.sort_by_key(|fix| fix.app_id);
                Ok(fixes)
            })();
            let _ = sender.send(result);
        });
    }

    fn poll_fixes_refresh(&mut self) {
        let Some(receiver) = self.fixes_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.fixes_receiver = None;
                match result {
                    Ok(fixes) => {
                        self.fixes = fixes;
                        self.fixes_loaded = true;
                        self.rebuild_fix_index();
                    }
                    Err(error) => {
                        self.fixes_loaded = true;
                        self.status = format!("Fix list unavailable: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.fixes_receiver = None,
        }
    }

    fn start_repacks_refresh(&mut self) {
        if self.repacks_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.repacks_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = ProxyClient::new()
                .and_then(|client| client.repacks())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn poll_repacks_refresh(&mut self) {
        let Some(receiver) = self.repacks_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.repacks_receiver = None;
                match result {
                    Ok(repacks) => {
                        self.repacks = repacks;
                        self.repacks_loaded = true;
                        self.rebuild_repack_index();
                    }
                    Err(error) => {
                        self.repacks_loaded = true;
                        self.status = format!("Repack list unavailable: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.repacks_receiver = None,
        }
    }

    /// The available repack sources for an app, if any.
    fn repack_for(&self, app_id: u32) -> Option<&RepackApp> {
        self.repacks.iter().find(|repack| repack.app_id == app_id)
    }

    /// The available fix for an app, if any.
    fn fix_for(&self, app_id: u32) -> Option<&FixEntry> {
        self.fixes.iter().find(|fix| fix.app_id == app_id)
    }

    /// Rebuilds the repack lookup (app -> repacker names) and the distinct-repacker list the Home
    /// filter dropdown offers. Called whenever the repack list is (re)loaded.
    fn rebuild_repack_index(&mut self) {
        let mut by_app = std::collections::HashMap::with_capacity(self.repacks.len());
        let mut names = std::collections::BTreeSet::new();
        for repack in &self.repacks {
            let mut lowered = Vec::with_capacity(repack.sources.len());
            for source in &repack.sources {
                names.insert(source.repacker.clone());
                lowered.push(source.repacker.to_lowercase());
            }
            by_app.insert(repack.app_id, lowered);
        }
        self.repackers_by_app = by_app;
        self.available_repackers = names.into_iter().collect();
        // Drop a now-missing repacker selection so the filter can never point at nothing.
        if let RepackFilter::Repacker(name) = &self.repack_filter
            && !self.available_repackers.iter().any(|entry| entry == name)
        {
            self.repack_filter = RepackFilter::Any;
        }
    }

    /// Rebuilds the fix lookup (the set of apps with a Denuvo fix available). Called on fix reload.
    fn rebuild_fix_index(&mut self) {
        self.fix_flags_by_app = self
            .fixes
            .iter()
            .filter(|fix| fix.denuvo.is_some())
            .map(|fix| fix.app_id)
            .collect();
    }

    /// Downloads the GitHub Denuvo fix (build-locked Lua + zip parts, each SHA-verified), installs
    /// the Lua into the plug-in folder — replacing any other Lua for the app — and extracts the zip
    /// over the game's install folder, on a background thread.
    fn apply_denuvo_fix_for(&mut self, app_id: u32) {
        if self.background_action.is_some() {
            return;
        }
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        let Some(install_dir) = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(SteamManifest::install_dir)
        else {
            self.status = "Install the game through Steam before applying its fix.".into();
            self.status_error = true;
            return;
        };
        let Some(denuvo) = self.fix_for(app_id).and_then(|fix| fix.denuvo.clone()) else {
            return;
        };
        // A build-locked fix breaks on a Steam update, so block updates once it is applied.
        self.pending_fix_block = Some(app_id);
        self.status = "Downloading and applying the Denuvo fix…".into();
        self.status_error = false;
        self.busy_label = Some("Applying the fix (this can take a while for large fixes)…".into());
        let (sender, receiver) = mpsc::channel();
        self.background_action = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let client = ProxyClient::new().map_err(|error| error.to_string())?;
                let lua = client
                    .fetch_file(&denuvo.lua)
                    .map_err(|error| error.to_string())?;
                // Reassemble the (possibly split) zip parts, in order, into a temp file so a
                // multi-hundred-MB fix never has to be held whole in memory.
                let temp_dir = std::env::temp_dir().join("Drydock").join("fixes");
                std::fs::create_dir_all(&temp_dir).map_err(|error| error.to_string())?;
                let zip_path = temp_dir.join(format!("{app_id}-{}.zip", std::process::id()));
                {
                    let mut file = std::fs::File::create(&zip_path).map_err(|error| error.to_string())?;
                    for part in &denuvo.zip_parts {
                        let bytes = client.fetch_fix_part(part).map_err(|error| error.to_string())?;
                        std::io::Write::write_all(&mut file, &bytes).map_err(|error| error.to_string())?;
                    }
                }
                let outcome = apply_denuvo_fix(&root, &install_dir, &denuvo, &lua, &zip_path)
                    .map_err(|error| error.to_string());
                let _ = std::fs::remove_file(&zip_path);
                let count = outcome?;
                Ok(format!(
                    "Denuvo fix applied — verified unlock installed, {count} game files replaced."
                ))
            })();
            let _ = sender.send(result);
        });
    }

    /// Verifies a chosen game folder before a request code is generated: fetches the app's Steam
    /// launch executables, resolves the real install root by their relative path, then scans the
    /// root for crack/HV artifacts. On a clean folder it proceeds straight to code generation.
    fn start_activation_check(&mut self, app_id: u32, chosen: PathBuf) {
        if self.activation_check_receiver.is_some()
            || self.activation_receiver.is_some()
            || self.activation_remove_receiver.is_some()
        {
            return;
        }
        self.activation_request_code.clear();
        self.activation_root = None;
        self.verified_entitlement = None;
        self.entitlement_success_app = None;
        self.response_code = std::array::from_fn(|_| String::new());
        self.status = "Verifying the game folder…".into();
        self.status_error = false;
        self.busy_label = Some("Verifying the game folder…".into());
        self.activation_check_app = Some(app_id);
        let (sender, receiver) = mpsc::channel();
        self.activation_check_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| -> Result<ActivationCheck, String> {
                if !chosen.is_dir() {
                    return Err("Select the game's folder first.".to_owned());
                }
                let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
                if executables.is_empty() {
                    return Err(
                        "Steam lists no launch executable for this game to verify against.".to_owned(),
                    );
                }
                let Some(root) = resolve_game_root(&chosen, &executables) else {
                    return Err(
                        "These files don't look like the selected game. Pick the correct game folder."
                            .to_owned(),
                    );
                };
                let files = scan_crack_files(&root);
                if files.is_empty() {
                    Ok(ActivationCheck::Ready { root })
                } else {
                    Ok(ActivationCheck::NeedsRemoval { root, files })
                }
            })();
            let _ = sender.send(result);
        });
    }

    fn poll_activation_check(&mut self) {
        let Some(receiver) = self.activation_check_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_check_receiver = None;
                self.busy_label = None;
                let app_id = self.activation_check_app.take();
                match result {
                    Ok(ActivationCheck::Ready { root }) => {
                        self.activation_path = root.display().to_string();
                        self.activation_root = Some(root);
                        if let Some(app_id) = app_id {
                            self.generate_activation_request(app_id);
                        }
                    }
                    Ok(ActivationCheck::NeedsRemoval { root, files }) => {
                        self.activation_path = root.display().to_string();
                        if let Some(app_id) = app_id {
                            self.pending_crack = Some(PendingCrack { app_id, root, files });
                        }
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.activation_check_receiver = None,
        }
    }

    /// Deletes the crack/HV artifacts the check found, then continues to code generation.
    fn start_crack_removal(&mut self) {
        if self.activation_remove_receiver.is_some() {
            return;
        }
        let Some(PendingCrack { app_id, root, files }) = self.pending_crack.take() else {
            return;
        };
        self.status = "Removing crack files…".into();
        self.status_error = false;
        self.busy_label = Some("Removing crack files…".into());
        let (sender, receiver) = mpsc::channel();
        self.activation_remove_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = remove_paths(&files)
                .map(|()| (app_id, root))
                .map_err(|error| format!("Could not remove the crack files: {error}"));
            let _ = sender.send(result);
        });
    }

    fn poll_activation_remove(&mut self) {
        let Some(receiver) = self.activation_remove_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_remove_receiver = None;
                self.busy_label = None;
                match result {
                    Ok((app_id, root)) => {
                        self.activation_path = root.display().to_string();
                        self.activation_root = Some(root);
                        self.status = "Crack files removed.".into();
                        self.status_error = false;
                        self.generate_activation_request(app_id);
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.activation_remove_receiver = None,
        }
    }

    fn generate_activation_request(&mut self, app_id: u32) {
        if self.activation_receiver.is_some() {
            return;
        }
        self.activation_request_code.clear();
        self.verified_entitlement = None;
        self.entitlement_success_app = None;
        self.response_code = std::array::from_fn(|_| String::new());
        self.busy_label = Some("Creating a protected activation request…".into());
        let settings_directory = self.paths.settings_dir();
        let (sender, receiver) = mpsc::channel();
        self.activation_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = ActivationRequestService::new(settings_directory)
                .and_then(|service| service.generate_delivery_code(app_id))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn verify_activation_response(&mut self, app_id: u32) {
        if self.activation_verify_receiver.is_some() {
            return;
        }
        // The verified payload installs into the game folder resolved when the request code was
        // generated (a Steam install or a foreign one).
        let Some(target_root) = self.activation_root.clone() else {
            self.status = "Verify the game folder and generate a request code first.".into();
            self.status_error = true;
            return;
        };
        let response_code = self.response_code.concat();
        self.verified_entitlement = None;
        self.entitlement_success_app = None;
        self.busy_label = Some("Verifying and installing the activation…".into());
        let settings_directory = self.paths.settings_dir();
        let (sender, receiver) = mpsc::channel();
        self.activation_verify_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = ActivationRequestService::new(settings_directory)
                .and_then(|service| service.download_and_install(&response_code, app_id, &target_root))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn poll_activation_verification(&mut self) {
        let Some(receiver) = self.activation_verify_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_verify_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(entitlement) => {
                        match self.protect_activated_manifest(entitlement.app_id) {
                            Ok(()) => {
                                self.entitlement_success_app = Some(
                                    self.catalog
                                        .iter()
                                        .find(|app| app.app_id == entitlement.app_id)
                                        .map_or_else(
                                            || format!("App {}", entitlement.app_id),
                                            |app| app.name.clone(),
                                        ),
                                );
                                self.status = format!(
                                    "Activation applied for App {} — game files installed and Steam updates blocked.",
                                    entitlement.app_id
                                );
                                self.status_error = false;
                            }
                            Err(error) => {
                                self.status = format!(
                                    "Activation was verified, but Steam updates could not be blocked: {error}"
                                );
                                self.status_error = true;
                            }
                        }
                        self.verified_entitlement = Some(entitlement);
                    }
                    Err(error) => {
                        self.status = format!("Activation failed: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.activation_verify_receiver = None;
                self.busy_label = None;
                self.status = "Activation verification ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn protect_activated_manifest(&mut self, app_id: u32) -> Result<(), String> {
        let manifest = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .ok_or_else(|| "the Steam manifest is no longer available".to_owned())?;
        set_manifest_updates_enabled(&manifest.manifest_path, false).map_err(|error| error.to_string())?;
        self.settings.steam_updates_enabled.insert(app_id, false);
        self.settings
            .save(&self.paths.settings_file())
            .map_err(|error| error.to_string())
    }

    fn start_update_check(&mut self, automatic: bool) {
        if self.update_receiver.is_some() || !AppUpdater::can_self_update() {
            return;
        }
        let repository = AppUpdater::configured_repository().unwrap_or_default();
        let access_mode = github_access_mode();
        let marker_value = format!("{APP_VERSION}:{repository}:{access_mode}");
        let marker = self.paths.cache_dir().join("update-check.marker");
        if automatic && refresh_marker_is_current(&marker, &marker_value, UPDATE_CHECK_COOLDOWN) {
            return;
        }
        if let Some(parent) = marker.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(marker, marker_value);
        self.status = "Checking for a verified Drydock update…".into();
        self.status_error = false;
        self.busy_label = Some("Checking the release channel…".into());
        let (sender, receiver) = mpsc::channel();
        self.update_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = AppUpdater::new()
                .and_then(|updater| updater.prepare_update())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn poll_update(&mut self) {
        let Some(receiver) = self.update_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.update_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(Some(update)) => match AppUpdater::launch(&update) {
                        Ok(()) => {
                            self.status = format!("Installing Drydock {}…", update.version);
                            self.status_error = false;
                            self.exit_for_update = true;
                        }
                        Err(error) => {
                            self.status = format!("Update could not be started: {error}");
                            self.status_error = true;
                        }
                    },
                    Ok(None) => {
                        self.status = "Drydock is up to date".into();
                        self.status_error = false;
                    }
                    Err(error) => {
                        self.status = format!("Update check failed: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.update_receiver = None;
                self.busy_label = None;
                self.status = "The update check ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn poll_activation_request(&mut self) {
        let Some(receiver) = self.activation_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(code) => {
                        self.status = if is_short_activation_code(&code) {
                            "Short activation request created".into()
                        } else {
                            "The short-code service is unavailable. A complete fallback request was created."
                                .into()
                        };
                        self.status_error = false;
                        self.activation_request_code = code;
                    }
                    Err(error) => {
                        self.status = format!("Activation request failed: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.activation_receiver = None;
                self.busy_label = None;
                self.status = "The activation request ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn open_details(&mut self, app_id: u32) {
        self.page = Page::Details;
        self.details_app_id = Some(app_id);
        self.store_details = None;
        self.store_loading = true;
        self.screenshot_index = 0;

        let cache_directory = self.paths.cache_dir().join("store-details");
        let (sender, receiver) = mpsc::channel();
        self.store_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = SteamStoreClient::new(cache_directory)
                .and_then(|client| client.details(app_id))
                .map_err(|error| error.to_string());
            let _ = sender.send((app_id, result));
        });
    }

    /// Starts a background depot download (or verify) into the Steam library. Only one job runs at a
    /// time. The install root is the installed game's folder when present, otherwise
    /// `steamapps/common/<installdir>` under the main Steam library (installdir resolved on the thread).
    /// Whether a background download/verify thread is currently running.
    fn download_running(&self) -> bool {
        self.download_job
            .as_ref()
            .is_some_and(|job| job.finished.is_none())
    }

    /// Spawns the background thread for one depot job and makes it the active `download_job`. Callers
    /// (queue start / verify) guarantee nothing else is running.
    fn spawn_job(&mut self, app_id: u32, name: String, kind: DownloadKind) {
        let steam_root = self.steam.root.clone();
        let installed_dir = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(SteamManifest::install_dir);
        // Parallel connections (0 = fall back to the default 8) and an optional MB/s cap, from Settings.
        let connections = match self.settings.max_download_connections {
            0 => 8,
            n => n.clamp(1, 32),
        } as usize;
        let max_bps = match self.settings.max_download_mbps {
            0 => None,
            mbps => Some(u64::from(mbps) * 1024 * 1024),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let job_name = name.clone();
        std::thread::spawn(move || {
            let result = run_depot_job(
                app_id,
                &job_name,
                kind,
                steam_root,
                installed_dir,
                connections,
                max_bps,
                &thread_cancel,
                &sender,
            );
            let _ = sender.send(DownloadUpdate::Finished(result));
        });
        self.download_job = Some(DownloadJob {
            app_id,
            name,
            kind,
            cancel,
            receiver,
            progress: None,
            speed_bps: 0.0,
            peak_bps: 0.0,
            sample: None,
            finished: None,
        });
    }

    /// Applies a [`QueueEffect`] from [`drydock_core::download_queue`]: persist the reordered queue and
    /// then start, switch or leave the worker thread alone as the effect requires.
    ///
    /// The ordering rules themselves live in core (and are unit-tested there); this is only the part
    /// that owns threads and the settings file.
    fn apply_queue_effect(&mut self, effect: QueueEffect, switching_status: &str) {
        match effect {
            QueueEffect::Unchanged => {}
            QueueEffect::PersistOnly => {
                let _ = self.persist_settings();
            }
            QueueEffect::StartFront => {
                let _ = self.persist_settings();
                self.download_paused = false;
                self.download_error = None;
                self.start_front_download();
            }
            QueueEffect::SwitchToFront => {
                let _ = self.persist_settings();
                self.switch_or_start(switching_status);
            }
        }
    }

    /// Adds a game to the persistent download queue and starts it when nothing else is downloading.
    /// A game already in the queue isn't added twice.
    fn enqueue_download(&mut self, app_id: u32, name: String) {
        let busy = self.download_running() || self.download_paused;
        let effect = download_queue::enqueue(
            &mut self.settings.download_queue,
            QueuedDownload {
                app_id,
                name: name.clone(),
            },
            busy,
        );
        match effect {
            QueueEffect::Unchanged => {
                self.status = format!("{name} is already in the download queue.");
                self.status_error = false;
            }
            QueueEffect::PersistOnly => {
                let _ = self.persist_settings();
                self.status = format!("Queued {name} for download.");
                self.status_error = false;
            }
            _ => self.apply_queue_effect(effect, "Switching download…"),
        }
    }

    /// Starts (or resumes) the download at the front of the queue. The depot engine skips chunks that
    /// already verify on disk, so this resumes an interrupted download where it left off.
    fn start_front_download(&mut self) {
        if self.download_running() {
            return;
        }
        let Some(front) = self.settings.download_queue.first().cloned() else {
            return;
        };
        self.download_paused = false;
        self.download_error = None;
        self.spawn_job(front.app_id, front.name.clone(), DownloadKind::Download);
        self.status = format!("Downloading {}…", front.name);
        self.status_error = false;
    }

    /// Runs a one-off verify (not queued/persisted) when nothing is downloading.
    fn start_verify(&mut self, app_id: u32, name: String) {
        if self.download_running() {
            self.status = "A download is already in progress.".into();
            self.status_error = true;
            return;
        }
        self.spawn_job(app_id, name, DownloadKind::Verify);
        self.status = "Verifying files…".into();
        self.status_error = false;
    }

    /// Pauses the running download: the thread stops cleanly between chunks and the entry stays at the
    /// front of the queue, so partial files remain and it can resume later.
    fn pause_download(&mut self) {
        if let Some(job) = self.download_job.as_ref()
            && job.kind == DownloadKind::Download
            && job.finished.is_none()
        {
            job.cancel.store(true, Ordering::Relaxed);
            self.download_paused = true;
            self.status = "Pausing the download…".into();
            self.status_error = false;
        }
    }

    /// Removes the current (paused) download from the queue and starts the next one, if any. Partial
    /// files are left on disk. Only meaningful when the front download is paused (no thread running).
    fn remove_current_download(&mut self) {
        let effect = download_queue::remove_front(&mut self.settings.download_queue);
        self.apply_queue_effect(effect, "Switching download…");
    }

    /// Removes a queued (not-current) download by App ID.
    fn remove_queued_download(&mut self, app_id: u32) {
        let effect = download_queue::remove_queued(&mut self.settings.download_queue, app_id);
        self.apply_queue_effect(effect, "Switching download…");
    }

    /// Makes a queued download the current one: moves it to the front and starts it. Whatever was
    /// downloading is stopped and stays in the queue right behind it (it resumes when it reaches the
    /// front again).
    fn activate_download(&mut self, app_id: u32) {
        let running = self.download_running();
        let effect = download_queue::activate(&mut self.settings.download_queue, app_id, running);
        self.apply_queue_effect(effect, "Switching download…");
    }

    /// Sends the active download to the back of the queue and starts the next one. With nothing else
    /// queued, this just pauses it.
    fn demote_current_download(&mut self) {
        let running = self.download_running();
        let effect = download_queue::demote_front(&mut self.settings.download_queue, running);
        if effect == QueueEffect::Unchanged {
            // Fewer than two entries — there is no "back" to move to, so pausing is the useful action.
            self.pause_download();
            return;
        }
        self.apply_queue_effect(effect, "Moving to the back of the queue…");
    }

    /// After the queue was reordered: stop the running thread so the new front starts (a "switch",
    /// not a pause), or start the new front directly when nothing is running.
    fn switch_or_start(&mut self, switching_status: &str) {
        if self.download_running() {
            self.download_switch_pending = true;
            if let Some(job) = &self.download_job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.status = switching_status.to_owned();
            self.status_error = false;
        } else {
            self.download_paused = false;
            self.download_error = None;
            self.start_front_download();
        }
    }

    fn poll_download(&mut self) {
        let mut finished: Option<Result<String, String>> = None;
        if let Some(job) = self.download_job.as_mut() {
            if job.finished.is_some() {
                return;
            }
            loop {
                match job.receiver.try_recv() {
                    Ok(DownloadUpdate::Progress(progress)) => job.progress = Some(progress),
                    Ok(DownloadUpdate::Finished(result)) => {
                        finished = Some(result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some(Err("The download ended unexpectedly.".into()));
                        break;
                    }
                }
            }
            // Estimate download speed from how many bytes arrived since the last ~0.5s sample.
            if let Some(done) = job.progress.as_ref().map(|p| p.done_bytes) {
                let now = Instant::now();
                match job.sample {
                    Some((then, prev_done)) if now.duration_since(then).as_secs_f64() >= 0.5 => {
                        let elapsed = now.duration_since(then).as_secs_f64();
                        let instant = done.saturating_sub(prev_done) as f64 / elapsed;
                        // Exponential smoothing so the number doesn't jump around.
                        job.speed_bps = if job.speed_bps == 0.0 {
                            instant
                        } else {
                            job.speed_bps * 0.6 + instant * 0.4
                        };
                        job.peak_bps = job.peak_bps.max(job.speed_bps);
                        job.sample = Some((now, done));
                    }
                    None => job.sample = Some((now, done)),
                    _ => {}
                }
            }
            if let Some(result) = finished.clone() {
                job.finished = Some(result);
            }
        }
        // Remember the latest progress so a paused download can still show its position.
        if let Some(progress) = self.download_job.as_ref().and_then(|job| job.progress.clone()) {
            self.download_last = Some(progress);
        }
        if let Some(result) = finished {
            let kind = self.download_job.as_ref().map(|job| job.kind);
            match &result {
                Ok(message) => {
                    self.status = message.clone();
                    self.status_error = false;
                }
                Err(message) => {
                    self.status = message.clone();
                    self.status_error = true;
                }
            }
            if kind == Some(DownloadKind::Download) {
                let was_pause = self.download_paused;
                let switching = std::mem::take(&mut self.download_switch_pending);
                let finished_game = self
                    .download_job
                    .as_ref()
                    .map(|job| (job.app_id, job.name.clone()));
                self.download_job = None; // the download thread has ended
                match result {
                    Ok(_) => {
                        // Completed: drop it from the queue by App ID (robust to reordering), persist,
                        // register it in the Drydock library, and resume the next one.
                        if let Some((id, name)) = finished_game {
                            download_queue::remove_completed(&mut self.settings.download_queue, id);
                            let _ = self.persist_settings();
                            self.start_download_install_detect(id, name);
                        }
                        self.download_paused = false;
                        self.download_error = None;
                        self.refresh_dynamic_state();
                        self.start_front_download();
                    }
                    Err(message) => {
                        if switching {
                            // Cancelled only to switch to a reordered front — the game stays queued at
                            // its new position; start whatever is now at the front.
                            self.download_paused = false;
                            self.download_error = None;
                            self.start_front_download();
                        } else {
                            // A user pause or a real error: keep the entry so it can resume; record the
                            // message only when it wasn't a deliberate pause.
                            self.download_paused = true;
                            self.download_error = if was_pause { None } else { Some(message) };
                        }
                    }
                }
            } else {
                // A one-off verify leaves its result in `download_job` (the banner shows DISMISS).
                self.refresh_dynamic_state();
            }
        }
    }

    fn poll_store_details(&mut self) {
        let Some(receiver) = self.store_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok((app_id, result)) => {
                self.store_receiver = None;
                if self.details_app_id != Some(app_id) {
                    return;
                }
                self.store_loading = false;
                match result {
                    Ok(details) => {
                        self.status = if details.stale_cache {
                            format!(
                                "Loaded cached {} details; Steam Store is unavailable",
                                details.name
                            )
                        } else {
                            format!("Loaded {}", details.name)
                        };
                        self.status_error = details.stale_cache;
                        self.store_details = Some(details);
                    }
                    Err(error) => {
                        self.status = format!("Store details could not be loaded: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.store_receiver = None;
                self.store_loading = false;
                self.status = "The store request ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Kicks off the live storefront fetch (Steam's `featuredcategories`) unless one is already in
    /// flight or a fresh result is in hand. Called when the Store page first needs it.
    fn start_featured(&mut self) {
        if self.featured_receiver.is_some() || self.featured.is_some() {
            return;
        }
        self.featured_loading = true;
        self.featured_error = None;
        let cache_directory = self.paths.cache_dir().join("store-details");
        let (sender, receiver) = mpsc::channel();
        self.featured_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = SteamStoreClient::new(cache_directory)
                .and_then(|client| client.featured())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn poll_featured(&mut self) {
        let Some(receiver) = self.featured_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.featured_receiver = None;
                self.featured_loading = false;
                match result {
                    Ok(featured) => {
                        self.featured = Some(featured);
                        self.featured_error = None;
                    }
                    Err(error) => self.featured_error = Some(error),
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.featured_receiver = None;
                self.featured_loading = false;
                self.featured_error = Some("The store feed request ended unexpectedly".into());
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn restart_steam_in_background(&mut self) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.background_action.is_some() {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        self.background_action = Some(receiver);
        self.busy_label = Some("Restarting Steam…".into());
        std::thread::spawn(move || {
            let result = restart_steam(&root)
                .map(|()| "Steam restarted".to_owned())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn poll_background_action(&mut self) {
        let Some(receiver) = self.background_action.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.background_action = None;
                self.busy_label = None;
                let fix_block = self.pending_fix_block.take();
                match result {
                    Ok(status) => {
                        self.status = status;
                        self.status_error = false;
                        // Applying a fix succeeded — block Steam updates so the build-locked
                        // fix is not overwritten by a game update.
                        if let Some(app_id) = fix_block {
                            self.status = match self.protect_activated_manifest(app_id) {
                                Ok(()) => format!("{} Steam updates disabled for this game.", self.status),
                                Err(error) => {
                                    format!("{} (Updates could not be disabled: {error})", self.status)
                                }
                            };
                        }
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.background_action = None;
                self.busy_label = None;
                self.pending_fix_block = None;
                self.status = "The background action ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Refreshes the Steam Service status in the background (remote manifest + local files).
    fn refresh_service_status(&mut self) {
        let Some(root) = self.steam.root.clone() else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| {
                let manifest = OpenSteamTool::new()
                    .and_then(|ost| ost.manifest())
                    .map_err(|error| error.to_string())?;
                Ok(ServiceOutcome::Status {
                    status: service_status(&root, &manifest),
                    note: String::new(),
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Installs or updates the Steam Service: stops Steam, installs the verified package, restarts.
    fn install_steam_service(&mut self) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let reinstall = matches!(
            self.service_status.as_ref().map(|status| status.state),
            Some(SteamServiceState::Current)
        );
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some(
            if reinstall {
                "Reinstalling the Steam Service…"
            } else {
                "Installing the Steam Service…"
            }
            .into(),
        );
        std::thread::spawn(move || {
            let result = (|| {
                let package = OpenSteamTool::new()
                    .and_then(|ost| ost.download_package())
                    .map_err(|error| error.to_string())?;
                stop_steam(&root).map_err(|error| error.to_string())?;
                let status = install_service(&root, &package).map_err(|error| error.to_string())?;
                let note = match start_steam(&root) {
                    Ok(()) => "Steam Service installed. Steam started.".to_owned(),
                    Err(error) => {
                        format!("The Steam Service was installed, but Steam could not be restarted: {error}")
                    }
                };
                Ok(ServiceOutcome::Status { status, note })
            })();
            let _ = sender.send(result);
        });
    }

    /// Removes the Steam Service: stops Steam, deletes the service files, restarts.
    fn uninstall_steam_service(&mut self) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some("Removing the Steam Service…".into());
        std::thread::spawn(move || {
            let result = (|| {
                stop_steam(&root).map_err(|error| error.to_string())?;
                let status = uninstall_service(&root).map_err(|error| error.to_string())?;
                let note = match start_steam(&root) {
                    Ok(()) => "Steam Service removed. Steam started.".to_owned(),
                    Err(error) => {
                        format!("The Steam Service was removed, but Steam could not be restarted: {error}")
                    }
                };
                Ok(ServiceOutcome::Status { status, note })
            })();
            let _ = sender.send(result);
        });
    }

    /// Downloads and installs the Lua unlock files for `app_id`. Steam is not restarted — the Steam
    /// Service picks the Lua up on its own.
    fn add_app_to_steam(&mut self, app_id: u32) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        if let Err(retry) = self.download_limiter.check() {
            self.status = format!(
                "Too many requests. Please wait {}s before adding another app.",
                retry.as_secs().max(1)
            );
            self.status_error = true;
            return;
        }
        let name = self.app_display_name(app_id);
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some(format!("Adding {name} to Steam…"));
        std::thread::spawn(move || {
            let result = (|| {
                let client = ProxyClient::new().map_err(|error| error.to_string())?;
                // Re-check the Steam Service is current before touching the plug-in folder.
                let manifest = OpenSteamTool::new()
                    .and_then(|ost| ost.manifest())
                    .map_err(|error| error.to_string())?;
                if service_status(&root, &manifest).state != SteamServiceState::Current {
                    return Err("Install or update the Steam Service before adding an app.".to_owned());
                }
                let bytes = client.download_lua(app_id).map_err(|error| error.to_string())?;
                let file_name = format!("{app_id}.lua");
                let installed_names = vec![file_name.clone()];
                let mut payload = std::collections::BTreeMap::new();
                payload.insert(file_name, bytes);
                add_app_files(&root, &payload).map_err(|error| error.to_string())?;
                Ok(ServiceOutcome::Added {
                    app_id,
                    files: installed_names,
                    note: format!("\"{name}\" added to Steam."),
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Adds the GitHub build-locked "Denuvo fix" Lua to the Steam plug-in folder in place of the
    /// normal token Lua, pinning the game to the cracked build. Downloads and installs on a
    /// background thread; the game files themselves are applied separately via APPLY DENUVO FIX.
    fn add_cracked_to_steam(&mut self, app_id: u32) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let Some(denuvo) = self.fix_for(app_id).and_then(|fix| fix.denuvo.clone()) else {
            self.status = "No cracked (Denuvo) version is available for this game.".into();
            self.status_error = true;
            return;
        };
        if let Err(retry) = self.download_limiter.check() {
            self.status = format!(
                "Too many requests. Please wait {}s before adding another app.",
                retry.as_secs().max(1)
            );
            self.status_error = true;
            return;
        }
        let name = self.app_display_name(app_id);
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some(format!("Adding the cracked version of {name} to Steam…"));
        std::thread::spawn(move || {
            let result = (|| {
                let client = ProxyClient::new().map_err(|error| error.to_string())?;
                // Re-check the Steam Service is current before touching the plug-in folder.
                let manifest = OpenSteamTool::new()
                    .and_then(|ost| ost.manifest())
                    .map_err(|error| error.to_string())?;
                if service_status(&root, &manifest).state != SteamServiceState::Current {
                    return Err("Install or update the Steam Service before adding an app.".to_owned());
                }
                // The verified build-locked Lua replaces the normal token Lua for this app, so it is
                // the only unlock left in the plug-in folder.
                let bytes = client
                    .fetch_file(&denuvo.lua)
                    .map_err(|error| error.to_string())?;
                let file_name = format!("{app_id}.lua");
                let installed_names = vec![file_name.clone()];
                let mut payload = std::collections::BTreeMap::new();
                payload.insert(file_name, bytes);
                add_app_files(&root, &payload).map_err(|error| error.to_string())?;
                Ok(ServiceOutcome::Added {
                    app_id,
                    files: installed_names,
                    note: format!(
                        "Cracked version of \"{name}\" added to Steam. Apply the Denuvo fix, then restart Steam."
                    ),
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Opens the Activation tab with `app_id` preselected (from the details ACTIVATE button).
    fn go_to_activation(&mut self, app_id: u32) {
        self.select_activation_game(app_id);
        self.page = Page::Activation;
    }

    /// Selects a game for activation: fills the search field with its name (collapsing the result
    /// list), prefills the folder from Steam when installed, and resets any in-flight code state.
    fn select_activation_game(&mut self, app_id: u32) {
        let name = self
            .catalog
            .iter()
            .find(|entry| entry.app_id == app_id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| format!("APP {app_id}"));
        self.selected_app = Some(app_id);
        self.activation_search = name;
        self.activation_request_code.clear();
        self.activation_root = None;
        self.verified_entitlement = None;
        self.entitlement_success_app = None;
        self.response_code = std::array::from_fn(|_| String::new());
        self.activation_path = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(|manifest| manifest.install_dir().display().to_string())
            .unwrap_or_default();
    }

    /// Loads in-game language options from an arbitrary folder (searching its subfolders for the
    /// Steam-settings language files), for the Tools tab language changer.
    fn load_language_from(&mut self, directory: PathBuf) {
        self.language_options = None;
        self.language_directory = None;
        self.language_selection.clear();
        if directory.as_os_str().is_empty() {
            return;
        }
        match read_language_options(&directory) {
            Ok(Some(options)) => {
                self.language_selection = options
                    .current_language
                    .clone()
                    .or_else(|| options.languages.first().cloned())
                    .unwrap_or_default();
                self.status = format!("Found {} language option(s)", options.languages.len());
                self.status_error = false;
                self.language_directory = Some(directory);
                self.language_options = Some(options);
            }
            Ok(None) => {
                self.status = "No language files found in that folder.".into();
                self.status_error = true;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// Removes the Lua unlock files for `app_id`. Steam is not restarted — the Steam Service drops
    /// the app on its own.
    fn remove_app_from_steam(&mut self, app_id: u32) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let name = self.app_display_name(app_id);
        // Names to remove come from what we recorded when the app was added.
        let mut names: Vec<String> = self
            .settings
            .added_apps
            .get(&app_id)
            .map(|state| state.files.keys().cloned().collect())
            .unwrap_or_default();
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some(format!("Removing {name} from Steam…"));
        std::thread::spawn(move || {
            let result = (|| {
                // Fall back to the deterministic Ryuu file name when nothing was recorded.
                if names.is_empty() {
                    names = vec![format!("{app_id}.lua")];
                }
                remove_app_files(&root, &names).map_err(|error| error.to_string())?;
                let note = format!("\"{name}\" removed from Steam.");
                Ok(ServiceOutcome::Removed { app_id, note })
            })();
            let _ = sender.send(result);
        });
    }

    fn app_display_name(&self, app_id: u32) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.app_id == app_id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| format!("App {app_id}"))
    }

    /// Returns the Steam root path, or sets an error status and returns `None` if missing.
    fn steam_root_or_error(&mut self, message: &str) -> Option<PathBuf> {
        let root = self.steam.root.clone();
        if root.is_none() {
            self.status = message.to_owned();
            self.status_error = true;
        }
        root
    }

    fn poll_service_action(&mut self) {
        let Some(receiver) = self.service_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.service_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(ServiceOutcome::Status { status, note }) => {
                        if !note.is_empty() {
                            self.status = note;
                            self.status_error = false;
                        }
                        self.service_status = Some(status);
                    }
                    Ok(ServiceOutcome::Added { app_id, files, note }) => {
                        let mut state = AddedAppState::default();
                        for file in files {
                            state.files.insert(file, String::new());
                        }
                        self.settings.added_apps.insert(app_id, state);
                        self.status_error = self.persist_settings().is_err();
                        self.status = note;
                    }
                    Ok(ServiceOutcome::Removed { app_id, note }) => {
                        self.settings.added_apps.remove(&app_id);
                        self.status_error = self.persist_settings().is_err();
                        self.status = note;
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.service_receiver = None;
                self.busy_label = None;
                self.status = "The Steam Service action ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Re-reads the state that can change while the app is open — the installed Steam games and
    /// the Steam Service status — so switching pages reflects reality without an app restart.
    ///
    /// The Steam library scan is a cheap local `.acf` read and runs on every page switch; the
    /// Service status is network-backed and therefore throttled by [`SERVICE_RECHECK_COOLDOWN`].
    /// Re-reads the CloudRedirect DLL state and the configured provider into the cached snapshot the
    /// Cloud page renders from.
    ///
    /// Both were previously read straight out of the render closure, which meant `dll_status` hashed
    /// the whole DLL with SHA-256 and `current_provider` re-read and re-parsed `config.json` on every
    /// single frame — 60 times a second for as long as the page was open.
    fn refresh_cloud_state(&mut self) {
        self.cloud_dll_status = self.steam.root.as_deref().map(cloud::dll_status);
        self.cloud_provider = cloud::current_provider();
    }

    fn refresh_dynamic_state(&mut self) {
        self.refresh_cloud_state();
        if self.steam.root.is_none() {
            return;
        }
        if let Ok(manifests) = load_manifests(&self.steam) {
            self.manifests = manifests;
            if self
                .selected_app
                .is_some_and(|app_id| !self.manifests.iter().any(|manifest| manifest.app_id == app_id))
            {
                self.selected_app = None;
                self.activation_request_code.clear();
                self.response_code = std::array::from_fn(|_| String::new());
            }
        }
        let due = self
            .last_service_check
            .is_none_or(|at| at.elapsed() >= SERVICE_RECHECK_COOLDOWN);
        if due && self.service_receiver.is_none() {
            self.last_service_check = Some(Instant::now());
            self.refresh_service_status();
        }
    }

    fn refresh_steam(&mut self) {
        self.steam = discover_steam(path_if_present(&self.steam_directory_draft));
        self.conflicts = detect_conflicting_software(self.steam.root.as_deref());
        match load_manifests(&self.steam) {
            Ok(manifests) => {
                self.manifests = manifests;
                self.status = if self.steam.root.is_some() {
                    format!("Loaded {} installed Steam games", self.manifests.len())
                } else {
                    "Steam was not found. Select its folder in Settings.".into()
                };
                self.status_error = self.steam.root.is_none();
                for manifest in &self.manifests {
                    if let Some(enabled) = self.settings.steam_updates_enabled.get(&manifest.app_id).copied()
                    {
                        if let Err(error) = set_manifest_updates_enabled(&manifest.manifest_path, enabled) {
                            self.status = format!("A saved game update policy could not be applied: {error}");
                            self.status_error = true;
                            break;
                        }
                    } else if let Ok(enabled) = updates_enabled(&manifest.manifest_path) {
                        self.settings
                            .steam_updates_enabled
                            .insert(manifest.app_id, enabled);
                    }
                }
                if self
                    .selected_app
                    .is_some_and(|app_id| !self.manifests.iter().any(|manifest| manifest.app_id == app_id))
                {
                    self.selected_app = None;
                    self.activation_request_code.clear();
                    self.response_code = std::array::from_fn(|_| String::new());
                }
            }
            Err(error) => {
                self.manifests.clear();
                self.selected_app = None;
                self.activation_request_code.clear();
                self.response_code = std::array::from_fn(|_| String::new());
                self.verified_entitlement = None;
                self.entitlement_success_app = None;
                self.status = format!("Steam library could not be read: {error}");
                self.status_error = true;
            }
        }
        self.service_status = None;
        self.refresh_service_status();
    }

    /// Validates the Steam directory draft text field, returning a cached result.
    ///
    /// The cache key is the trimmed draft string; stat() only runs when the user
    /// actually changes the text. The check verifies:
    ///   1. The path exists on disk.
    ///   2. The platform Steam launcher is present (`steam.exe` / `steam.sh`).
    ///   3. The `steamapps` directory is present (required for a usable root).
    fn validate_steam_draft(&mut self) -> SteamDirValidation {
        let draft = self.steam_directory_draft.trim().to_owned();
        if let Some((ref cached, ref result)) = self.validated_steam_path
            && cached == &draft
        {
            return result.clone();
        }
        let result = if draft.is_empty() {
            SteamDirValidation::Empty
        } else {
            let path = Path::new(&draft);
            if !path.is_dir() {
                SteamDirValidation::NotFound
            } else if !is_valid_steam_directory(Some(path)) {
                SteamDirValidation::MissingExecutable
            } else if !path.join("steamapps").is_dir() {
                SteamDirValidation::MissingSteamapps
            } else {
                SteamDirValidation::Valid
            }
        };
        self.validated_steam_path = Some((draft, result.clone()));
        result
    }

    fn save_settings(&mut self) -> bool {
        self.settings.steam_directory = self.steam_directory_draft.trim().to_owned();
        match self.write_settings() {
            Ok(()) => {
                self.status = "Settings saved".into();
                self.status_error = false;
                true
            }
            Err(error) => {
                self.status = format!("Settings could not be saved: {error}");
                self.status_error = true;
                false
            }
        }
    }

    /// Writes the settings to disk, refusing while the on-disk file is quarantined or unreadable.
    ///
    /// In that state the real `added_apps` / `installed_games` / `launch_paths` may still be in the
    /// file (or its quarantined copy) while `self.settings` holds defaults, so saving would destroy
    /// them. `discard_broken_settings` clears the block once the user has decided.
    fn write_settings(&self) -> Result<(), String> {
        if self.settings_read_only {
            return Err(
                "the settings file on disk is unreadable — saving is disabled so your library is not \
                 overwritten (Settings ▸ DISCARD BROKEN SETTINGS to start fresh)"
                    .to_owned(),
            );
        }
        self.settings
            .save(&self.paths.settings_file())
            .map_err(|error| error.to_string())
    }

    /// Persists the settings, **always** surfacing a failure in the status bar.
    ///
    /// Every queue/library mutation goes through here. Callers used to discard the result, so a
    /// failed write (full disk, antivirus lock, missing rights) left the user believing their
    /// change was stored while it only lived in memory until the next launch.
    fn persist_settings(&mut self) -> Result<(), String> {
        let result = self.write_settings();
        if let Err(error) = &result {
            self.status = ellipsize(&format!("Your change could not be saved: {error}"), 160);
            self.status_error = true;
        }
        result
    }

    /// Accepts the loss of an unreadable settings file and re-enables saving, starting from whatever
    /// is currently in memory (the defaults). The quarantined copy stays on disk either way.
    fn discard_broken_settings(&mut self) {
        self.settings_read_only = false;
        match self.write_settings() {
            Ok(()) => {
                self.status = "Started a fresh settings file. The unreadable one was kept beside it.".into();
                self.status_error = false;
            }
            Err(error) => {
                self.status = format!("A fresh settings file could not be written: {error}");
                self.status_error = true;
            }
        }
    }

    /// The top navigation bar (Steam-store style): the Drydock wordmark, the primary destinations as
    /// horizontal links, a live game search on the right, and a small overflow group for the
    /// secondary pages (Guide / Updates / Settings).
    fn top_nav(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("top_nav")
            .exact_size(TOP_NAV_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(20, 0))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(13, 15, 17))),
            )
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    // Brand mark: the avatar disc + the TIDE(S) wordmark, S in Steam blue.
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 13.0, ACCENT_DEEP);
                    egui::Image::new(egui::include_image!("../../../assets/app-icon.png"))
                        .corner_radius(13)
                        .paint_at(ui, rect);
                    ui.add_space(9.0);
                    ui.label(
                        RichText::new("Drydock")
                            .size(16.0)
                            .strong()
                            .color(TEXT)
                            .family(egui::FontFamily::Proportional),
                    );

                    ui.add_space(22.0);
                    // Primary destinations as underlined link tabs.
                    for (page, label) in [
                        (Page::Home, "STORE"),
                        (Page::Library, "LIBRARY"),
                        (Page::Activation, "ACTIVATION"),
                        (Page::Tools, "TOOLS"),
                        (Page::Cloud, "CLOUD"),
                    ] {
                        if nav_link(ui, label, self.page == page).clicked() {
                            self.page = page;
                        }
                        ui.add_space(4.0);
                    }

                    // Right side: search field, then the secondary-page overflow group.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        for (page, label) in [
                            (Page::Settings, "SETTINGS"),
                            (Page::Updates, "UPDATES"),
                            (Page::Guide, "HELP"),
                        ] {
                            if nav_link(ui, label, self.page == page).clicked() {
                                self.page = page;
                            }
                            ui.add_space(2.0);
                        }
                        ui.add_space(8.0);
                        // A rounded search pill; typing anything jumps to the Store's search results.
                        let before = self.search.clone();
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("Search…")
                                .desired_width(190.0)
                                .margin(egui::Margin {
                                    left: 32,
                                    right: 12,
                                    top: 7,
                                    bottom: 7,
                                }),
                        );
                        // A magnifying-glass icon painted at the pill's left (a font glyph rendered as
                        // tofu on the bundled font, so it's drawn as a circle + handle instead).
                        let center = egui::pos2(response.rect.left() + 17.0, response.rect.center().y);
                        let radius = 5.0;
                        let painter = ui.painter();
                        painter.circle_stroke(center, radius, Stroke::new(1.6, MUTED));
                        let d = radius * std::f32::consts::FRAC_1_SQRT_2;
                        painter.line_segment(
                            [
                                egui::pos2(center.x + d, center.y + d),
                                egui::pos2(center.x + d + 3.5, center.y + d + 3.5),
                            ],
                            Stroke::new(1.6, MUTED),
                        );
                        if response.changed() && self.search != before && !self.search.trim().is_empty() {
                            self.page = Page::Home;
                        }
                    });
                });
            });
    }

    /// A slim status strip pinned to the bottom of the window: the single most relevant live
    /// notification on the left (Steam/Service/conflict state or the latest activity), the app
    /// version on the right. Replaces the sidebar footer from the old layout.
    /// The Steam-style Downloads bar: shown above the status bar while a depot download or verify is
    /// running (or just finished), with the game name, a progress bar, and Cancel/Dismiss.
    /// The centred download status shown in the bottom bar. Always present (so the Downloads page is
    /// one click away), reading "Downloads" at rest and reflecting the active job otherwise. Per-job
    /// detail (name, %, speed) lives only on the Downloads page.
    fn download_status_label(&self) -> (&'static str, Color32) {
        if self.download_running() {
            ("Downloads active", ACCENT)
        } else if self.download_paused {
            ("Downloads paused", DANGER)
        } else if !self.settings.download_queue.is_empty() {
            ("Downloads queued", AMBER)
        } else {
            ("Downloads", MUTED)
        }
    }

    /// The full Downloads page, Steam-style: a hero banner of the current game, live network/peak
    /// speed tiles, the download + install/verify progress bars, and the (currently single-job) queue.
    fn downloads_page(&mut self, ui: &mut egui::Ui) {
        if back_button(ui, "Return to the store").clicked() {
            self.page = Page::Home;
            return;
        }
        ui.add_space(12.0);
        page_heading(ui, "Downloads");
        ui.add_space(16.0);

        // The banner shows a running/finished verify, or the current download (queue front) whether
        // running, paused, or stopped by an error. With neither, there is nothing to download.
        let verify = self
            .download_job
            .as_ref()
            .filter(|job| job.kind == DownloadKind::Verify);
        let front = self.settings.download_queue.first().cloned();
        if verify.is_none() && front.is_none() {
            panel(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("No active downloads.")
                        .size(13.5)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Open a game and press Download to fetch its files here.")
                        .size(11.5)
                        .color(MUTED),
                );
                ui.add_space(8.0);
                if ui.add(primary_button("BROWSE THE STORE")).clicked() {
                    self.page = Page::Home;
                }
            });
            return;
        }

        let is_verify = verify.is_some();
        let (app_id, name) = match verify {
            Some(job) => (job.app_id, job.name.clone()),
            None => {
                let front = front.as_ref().expect("front present");
                (front.app_id, front.name.clone())
            }
        };
        let active = self
            .download_job
            .as_ref()
            .filter(|job| job.finished.is_none() && job.app_id == app_id);
        let running = active.is_some();
        let verify_finished = verify.and_then(|job| job.finished.clone());
        let paused = !is_verify && !running;
        let error = if paused { self.download_error.clone() } else { None };
        // Progress from the live thread, else the last remembered tick (so a paused bar keeps its
        // position), matched by App ID so it never shows a different game's progress.
        let progress = active.and_then(|job| job.progress.clone()).or_else(|| {
            self.download_last
                .as_ref()
                .filter(|tick| tick.app_id == app_id)
                .cloned()
        });
        let speed = active.map_or(0.0, |job| job.speed_bps);
        let peak = active.map_or(0.0, |job| job.peak_bps);

        let queue_len = self.settings.download_queue.len();
        let mut pause_clicked = false;
        let mut demote_clicked = false;
        let mut resume_clicked = false;
        let mut remove_clicked = false;
        let mut cancel_clicked = false;
        let mut dismiss = false;

        let done = progress.as_ref().map_or(0, |p| p.done_bytes);
        let total = progress.as_ref().map_or(0, |p| p.total_bytes);
        let fraction = if total > 0 {
            (done as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // A wide banner like Steam's Downloads header: the game's hero art fills the card, a solid
        // info panel sits on the right (speed stats on top, the progress bar below), and the game
        // name is set over the art on the left. The panel is opaque with a soft shadow fading into
        // it, so there's no hard seam or stray rounded corner in the middle of the image.
        let width = ui.available_width();
        let banner_h = (width * 0.26).clamp(220.0, 290.0);
        let corner = egui::CornerRadius::same(12);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, banner_h), Sense::hover());
        // The `header.jpg`/`capsule` art matches the banner's art region far better than the very wide
        // `library_hero`, so cover-fitting it fills the region with almost no crop — and no borders.
        let urls = [
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/header.jpg"),
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/capsule_616x353.jpg"),
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/library_hero.jpg"),
        ];
        let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
        // The solid banner base (also the right-hand info panel), then the game art cover-fitted into
        // the left art region — filled edge to edge, no borders.
        ui.painter().rect_filled(rect, corner, SURFACE);
        let panel_w = (width * 0.5).clamp(380.0, 640.0);
        let split_x = rect.right() - panel_w;
        let art_rect = egui::Rect::from_min_max(rect.min, egui::pos2(split_x, rect.bottom()));
        paint_remote_image_cover_multi(
            ui,
            art_rect,
            &refs,
            egui::CornerRadius {
                nw: 12,
                ne: 0,
                sw: 12,
                se: 0,
            },
        );
        {
            let painter = ui.painter().with_clip_rect(art_rect);
            // A bottom band under the name so it stays legible over any art.
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - 58.0),
                    egui::pos2(split_x, rect.bottom()),
                ),
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 12,
                    se: 0,
                },
                Color32::from_rgba_unmultiplied(8, 12, 18, 150),
            );
            painter.text(
                egui::pos2(rect.left() + 22.0, rect.bottom() - 19.0),
                egui::Align2::LEFT_BOTTOM,
                &name,
                FontId::proportional(24.0),
                Color32::WHITE,
            );
        }

        // The info content, laid out inside the solid right-hand panel.
        let content = egui::Rect::from_min_max(
            egui::pos2(split_x + 26.0, rect.top() + 22.0),
            egui::pos2(rect.right() - 26.0, rect.bottom() - 20.0),
        );
        let stage_label = if is_verify {
            if verify_finished.is_some() {
                "Complete"
            } else {
                "Files are being verified…"
            }
        } else if running {
            "Data is downloading…"
        } else if error.is_some() {
            "Paused — download error"
        } else {
            "Paused"
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
            ui.horizontal(|ui| {
                download_mini_stat(ui, "NETWORK", &human_bps(speed), ACCENT_SOFT);
                ui.add_space(24.0);
                download_mini_stat(ui, "PEAK", &human_bps(peak), ACCENT);
                ui.add_space(24.0);
                download_mini_stat(ui, "TOTAL SIZE", &human_bytes(total), AMBER);
            });
            ui.add_space(14.0);
            ui.painter().line_segment(
                [
                    egui::pos2(content.left(), ui.cursor().top()),
                    egui::pos2(content.right(), ui.cursor().top()),
                ],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
            );
            ui.add_space(14.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new(stage_label).size(12.5).strong().color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} / {}", human_bytes(done), human_bytes(total)))
                            .size(12.0)
                            .color(MUTED),
                    );
                });
            });
            ui.add_space(8.0);
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(content.width())
                    .text(format!("{:.0}%", fraction * 100.0)),
            );
            if let Some(p) = &progress {
                if !p.current_file.is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new(tail(&p.current_file, 48)).size(10.5).color(MUTED));
                }
            } else if running {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                    ui.add_space(6.0);
                    ui.label(RichText::new("Preparing…").size(11.0).color(MUTED));
                });
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if is_verify {
                    match &verify_finished {
                        Some(Ok(message)) => {
                            if ui.add(primary_button("DISMISS")).clicked() {
                                dismiss = true;
                            }
                            ui.label(RichText::new(message).size(10.5).color(VERDIGRIS));
                        }
                        Some(Err(message)) => {
                            if ui.add(primary_button("DISMISS")).clicked() {
                                dismiss = true;
                            }
                            ui.label(RichText::new(message).size(10.5).color(DANGER));
                        }
                        None => {
                            if ui.add(ghost_button("CANCEL")).clicked() {
                                cancel_clicked = true;
                            }
                        }
                    }
                } else if running {
                    if ui.add(ghost_button("PAUSE")).clicked() {
                        pause_clicked = true;
                    }
                    if queue_len > 1
                        && ui
                            .add(ghost_button("TO QUEUE"))
                            .on_hover_text("Send this download to the back of the queue and start the next")
                            .clicked()
                    {
                        demote_clicked = true;
                    }
                } else {
                    if ui.add(ghost_button("REMOVE")).clicked() {
                        remove_clicked = true;
                    }
                    if ui.add(primary_button("RESUME")).clicked() {
                        resume_clicked = true;
                    }
                    if let Some(message) = &error {
                        ui.label(RichText::new(message).size(10.5).color(DANGER));
                    }
                }
            });
        });

        // UP NEXT: the queued downloads behind the current one, each removable.
        let upcoming: Vec<QueuedDownload> = self.settings.download_queue.iter().skip(1).cloned().collect();
        let mut remove_queued: Option<u32> = None;
        let mut activate_queued: Option<u32> = None;
        ui.add_space(22.0);
        section_label(ui, &format!("UP NEXT ({})", upcoming.len()));
        ui.add_space(10.0);
        if upcoming.is_empty() {
            panel(ui, |ui| {
                ui.label(RichText::new("No downloads are queued.").size(11.5).color(MUTED));
            });
        } else {
            for item in &upcoming {
                panel(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&item.name).size(12.5).color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(ghost_button("REMOVE")).clicked() {
                                remove_queued = Some(item.app_id);
                            }
                            if ui
                                .add(primary_button("ACTIVATE"))
                                .on_hover_text("Download this now (the current one goes back into the queue)")
                                .clicked()
                            {
                                activate_queued = Some(item.app_id);
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        }

        if pause_clicked {
            self.pause_download();
        }
        if demote_clicked {
            self.demote_current_download();
        }
        if resume_clicked {
            self.start_front_download();
        }
        if remove_clicked {
            self.remove_current_download();
        }
        if let Some(app_id) = activate_queued {
            self.activate_download(app_id);
        }
        if cancel_clicked {
            if let Some(job) = &self.download_job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.status = "Cancelling the verify…".into();
            self.status_error = false;
        }
        if dismiss {
            self.download_job = None;
        }
        if let Some(app_id) = remove_queued {
            self.remove_queued_download(app_id);
        }
    }

    fn status_bar(&mut self, root: &mut egui::Ui) {
        let (download_label, download_accent) = self.download_status_label();
        let mut open_downloads = false;
        egui::Panel::bottom("status_bar")
            .exact_size(STATUS_BAR_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(18, 0))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(13, 15, 17))),
            )
            .show(root, |ui| {
                let bar = ui.max_rect();
                // Centred download status, painted on top of the bar (painting, not a widget, so it
                // doesn't consume the panel's layout space). Always shown so the Downloads page is a
                // click away; brightens on hover.
                let hovered = ui.rect_contains_pointer(egui::Rect::from_center_size(
                    bar.center(),
                    Vec2::new(150.0, bar.height()),
                ));
                let color = if hovered {
                    lerp_color(download_accent, TEXT, 0.5)
                } else {
                    download_accent
                };
                let text_rect = ui.painter().text(
                    bar.center(),
                    egui::Align2::CENTER_CENTER,
                    download_label,
                    FontId::proportional(11.5),
                    color,
                );
                let response = ui.interact(text_rect, ui.id().with("dl_status"), Sense::click());
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if response.clicked() {
                    open_downloads = true;
                }
                ui.horizontal_centered(|ui| {
                    let notes = self.collect_notifications();
                    if let Some(note) = notes.first() {
                        // Keep the left status clear of the centred download label: cap the title so a
                        // long message (e.g. a crack summary) ellipsises instead of running into it.
                        let (dot, _) = ui.allocate_exact_size(Vec2::new(9.0, 9.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, note.accent);
                        ui.add_space(4.0);
                        let title = ellipsize(&note.title, 82);
                        let title_label = ui.label(RichText::new(&title).size(11.0).color(TEXT));
                        if title != note.title {
                            title_label.on_hover_text(&note.title);
                        }
                        if !note.detail.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(ellipsize(&note.detail, 60)).size(10.5).color(MUTED));
                        }
                        if notes.len() > 1 {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!("+{} more", notes.len() - 1))
                                    .size(10.0)
                                    .color(MUTED),
                            )
                            .on_hover_text(
                                notes
                                    .iter()
                                    .skip(1)
                                    .map(|note| note.title.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("v{APP_VERSION}")).size(10.0).color(MUTED));
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Developed with ♥ from gamers for gamers")
                                .size(10.0)
                                .color(MUTED),
                        );
                    });
                });
            });
        if open_downloads {
            self.page = Page::Downloads;
        }
    }

    /// Live status for the bottom bar, most-important-first: environment problems (Steam missing,
    /// Service problems, conflicting/tampering software), then the latest activity message, then the
    /// benign "Service ready" state. The status bar shows the first; the rest fold into a "+N more".
    fn collect_notifications(&self) -> Vec<Notification> {
        let mut errors: Vec<Notification> = Vec::new();
        let mut activity: Vec<Notification> = Vec::new();
        let mut benign: Vec<Notification> = Vec::new();

        if self.steam.root.is_none() {
            errors.push(Notification::error(
                "Steam not found",
                "Set the Steam folder in Settings.",
            ));
        }

        match self.service_status.as_ref().map(|status| status.state) {
            Some(SteamServiceState::NotInstalled) => errors.push(Notification::warn(
                "Steam Service not installed",
                "Install it in Settings to enable Add to Steam.",
            )),
            Some(SteamServiceState::UpdateAvailable) => errors.push(Notification::warn(
                "Steam Service out of date",
                "Reinstall it in Settings to update.",
            )),
            Some(SteamServiceState::Error) => errors.push(Notification::error(
                "Steam Service problem",
                self.service_status
                    .as_ref()
                    .map_or("", |status| status.message.as_str()),
            )),
            Some(SteamServiceState::Current) => {
                benign.push(Notification::ok("Steam Service ready", ""));
            }
            None if self.service_receiver.is_some() => {
                benign.push(Notification::info("Checking Steam Service…", ""));
            }
            None => {}
        }

        for name in &self.conflicts.names {
            let detail = if name == "Modified Steam files" {
                "Foreign backup files were found in the Steam folder. They can break the Steam Service."
            } else {
                "Manages the same Steam files and can break activation. Remove it, then restart Steam."
            };
            errors.push(Notification::error(name, detail));
        }

        let status = self.status.trim();
        if !status.is_empty() {
            activity.push(if self.status_error {
                Notification::error(status, "")
            } else {
                Notification::info(status, "")
            });
        }

        errors.extend(activity);
        errors.extend(benign);
        errors
    }

    /// Home: a single centred search over the whole catalogue. Picking a game opens its details —
    /// the one place to "find everything". Fixes, cracks and repacks all live on the details page.
    /// The Store: a Steam-style storefront. The nav search drives a full-catalogue results list;
    /// with an empty query it shows the sub-tabbed featured storefront (a live feed of Steam's
    /// Featured / New Releases, filtered to the proxy gamelist, plus Drydock-only Repacks and Denuvo
    /// tabs), each capsule flagged when Drydock can activate it.
    fn home_page(&mut self, ui: &mut egui::Ui) {
        if !self.search.trim().is_empty() {
            self.store_search_results(ui);
            return;
        }
        self.start_featured();
        // The shell already caps every page to `CONTENT_WIDTH` (the hero image's width) and centres
        // it, so the banner fills with no side bars and the list lines up under it — no extra column
        // needed here.
        self.home_store_column(ui);
    }

    /// The storefront column body: sub-tabs, divider and the selected tab's content.
    fn home_store_column(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        // Steam-style store sub-tabs.
        let mut selected = self.store_tab;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (tab, label) in StoreTab::ALL {
                if store_subtab(ui, label, selected == tab).clicked() {
                    selected = tab;
                }
            }
        });
        if selected != self.store_tab {
            self.store_tab = selected;
            // Same reasoning as the page switch: the previous tab's rows are gone, so its queued
            // header lookups should not keep consuming the shared Steam request budget.
            self.header_resolver.cancel_pending();
        }
        ui.add_space(2.0);
        let line_y = ui.cursor().top();
        ui.painter()
            .hline(ui.max_rect().x_range(), line_y, Stroke::new(1.0, BORDER));
        ui.add_space(18.0);

        // The whole storefront reads from an owned snapshot so the render helpers can borrow `self`
        // immutably for the Activatable check without fighting the feed borrow.
        let featured = self.featured.clone();
        let action = match self.store_tab {
            StoreTab::Featured => self.store_featured(ui, featured.as_ref()),
            StoreTab::NewReleases => {
                self.store_shelf(ui, featured.as_ref().map(|feed| feed.new_releases.as_slice()))
            }
            StoreTab::Repacks => self.store_repacks(ui),
            StoreTab::DenuvoWatch => self.store_denuvo_watch(ui),
        };
        match action {
            Some(StoreAction::Details(app_id)) => self.open_details(app_id),
            Some(StoreAction::Activate(app_id)) => self.go_to_activation(app_id),
            None => {}
        }
    }

    /// A small "loading / offline" line shared by the storefront tabs while the live feed resolves.
    fn store_feed_status(&self, ui: &mut egui::Ui) -> bool {
        if self.featured.is_some() {
            return true;
        }
        if self.featured_loading {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Loading the live storefront from Steam…")
                        .size(12.5)
                        .color(MUTED),
                );
            });
        } else if let Some(error) = &self.featured_error {
            ui.label(
                RichText::new(format!("Steam storefront is unavailable: {error}"))
                    .size(12.5)
                    .color(AMBER),
            );
        }
        false
    }

    /// The Featured tab, laid out like Steam's store landing: a wide cinematic banner for the #1 top
    /// seller, then an "Featured & Recommended" grid of the rest, each with its Activatable overlay.
    fn store_featured(&self, ui: &mut egui::Ui, featured: Option<&StoreFeatured>) -> Option<StoreAction> {
        if !self.store_feed_status(ui) {
            return None;
        }
        let feed = featured?;
        if self.catalog.is_empty() {
            ui.label(RichText::new("Loading the game list…").color(MUTED));
            return None;
        }
        // Steam's ranked top sellers, narrowed to the games actually in our catalog (proxy gamelist),
        // so every card is one Drydock can get.
        let available: Vec<&StoreCapsule> = feed
            .top_sellers
            .iter()
            .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
            .collect();
        let mut items = available.into_iter();
        let Some(hero) = items.next() else {
            ui.label(
                RichText::new("None of Steam's top sellers are available in Drydock right now.").color(MUTED),
            );
            return None;
        };
        let mut action = None;
        // Full-width cinematic banner for the headline title. The storefront only ever offers "View"
        // — Activate lives on the details page, driven by the game's real Denuvo status.
        if let Some(hit) = store_banner(ui, hero, 1, false) {
            action = Some(hit);
        }

        ui.add_space(22.0);
        section_label(ui, "FEATURED & RECOMMENDED");
        ui.add_space(12.0);
        // The rest of the available top sellers as a price-free Steam-style list.
        ui.spacing_mut().item_spacing.y = 0.0;
        for capsule in items.take(20) {
            if let Some(hit) = store_list_row(ui, capsule, false) {
                action = Some(hit);
            }
        }
        action
    }

    /// A store category tab (New Releases): the live category as a price-free Steam-style list of
    /// rows.
    fn store_shelf(&self, ui: &mut egui::Ui, capsules: Option<&[StoreCapsule]>) -> Option<StoreAction> {
        if !self.store_feed_status(ui) {
            return None;
        }
        let capsules = capsules.unwrap_or_default();
        if capsules.is_empty() {
            ui.label(RichText::new("Nothing to show in this category right now.").color(MUTED));
            return None;
        }
        // Narrow the category to games actually in our catalog (proxy gamelist), so every row is
        // one Drydock can get.
        let available: Vec<&StoreCapsule> = capsules
            .iter()
            .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
            .collect();
        if available.is_empty() {
            ui.label(
                RichText::new("None of this category's games are available in Drydock right now.")
                    .color(MUTED),
            );
            return None;
        }
        let mut action = None;
        // No nested scroll area — the page's own scroll handles overflow (a scroll area inside the
        // centred zero-height content column collapses and clips the list).
        ui.spacing_mut().item_spacing.y = 0.0;
        for capsule in available {
            // Storefront rows are always "View"; Activate lives on the details page.
            if let Some(hit) = store_list_row(ui, capsule, false) {
                action = Some(hit);
            }
        }
        ui.add_space(24.0);
        action
    }

    /// The Drydock-only Denuvo Watch tab: the live set of games that actually use Denuvo — i.e. the
    /// ones that need Drydock to activate them — as a searchable list. Independent of the Steam feed.
    fn store_denuvo_watch(&self, ui: &mut egui::Ui) -> Option<StoreAction> {
        if !self.denuvo_loaded {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Loading the Denuvo watch list…")
                        .size(12.5)
                        .color(MUTED),
                );
            });
            return None;
        }
        ui.label(
            RichText::new(format!(
                "{} games currently ship with Denuvo — these are the titles Drydock activates.",
                group_thousands(self.denuvo_appids.len())
            ))
            .size(12.5)
            .color(MUTED),
        );
        ui.add_space(14.0);
        // Only Denuvo games we can name from the catalogue, alphabetised.
        let mut games: Vec<&CatalogApp> = self
            .catalog
            .iter()
            .filter(|entry| self.denuvo_appids.contains(&entry.app_id))
            .collect();
        games.sort_by_key(|entry| entry.name.to_lowercase());

        let mut action = None;
        // Same row structure as the Featured list — a fixed-height slot plus an explicit gap below —
        // so the spacing between cards is identical (a bare `item_spacing` doesn't take here).
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width();
        for entry in games.iter().take(400) {
            let selected = self.selected_app == Some(entry.app_id);
            let mut clicked = false;
            list_row_slot(ui, width, |ui| {
                clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
            });
            ui.add_space(LIST_ROW_GAP);
            if clicked {
                action = Some(StoreAction::Details(entry.app_id));
            }
        }
        action
    }

    /// The Repacks tab: every catalogue game Drydock has an external repack download for, as a
    /// searchable list. Independent of the Steam feed.
    fn store_repacks(&self, ui: &mut egui::Ui) -> Option<StoreAction> {
        if !self.repacks_loaded {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the repack list…").size(12.5).color(MUTED));
            });
            return None;
        }
        // Only games we can name from the catalogue that have at least one repack source.
        let mut games: Vec<&CatalogApp> = self
            .catalog
            .iter()
            .filter(|entry| self.repackers_by_app.contains_key(&entry.app_id))
            .collect();
        games.sort_by_key(|entry| entry.name.to_lowercase());

        if games.is_empty() {
            ui.label(RichText::new("No repacks available right now.").color(MUTED));
            return None;
        }
        ui.label(
            RichText::new(format!(
                "{} games have a repack download available.",
                group_thousands(games.len())
            ))
            .size(12.5)
            .color(MUTED),
        );
        ui.add_space(14.0);

        let mut action = None;
        // Same row structure as the Featured list (fixed slot + explicit gap) for identical spacing.
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width();
        for entry in games.iter().take(400) {
            let selected = self.selected_app == Some(entry.app_id);
            let mut clicked = false;
            list_row_slot(ui, width, |ui| {
                clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
            });
            ui.add_space(LIST_ROW_GAP);
            if clicked {
                action = Some(StoreAction::Details(entry.app_id));
            }
        }
        action
    }

    /// The nav-search results view: the whole catalogue filtered by the query and the repack/fix
    /// dropdowns, as a list of rows that open the details page.
    fn store_search_results(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        self.home_filter_row(ui);
        ui.add_space(12.0);
        let query = self.search.trim().to_lowercase();
        let open = {
            let repack_filter = self.repack_filter.clone();
            let fix_filter = self.fix_filter;
            let repackers = &self.repackers_by_app;
            let fix_flags = &self.fix_flags_by_app;
            let matches: Vec<&CatalogApp> = self
                .catalog
                .iter()
                .filter(|entry| {
                    catalog_matches_filters(entry, &repack_filter, fix_filter, repackers, fix_flags)
                        && (query.is_empty()
                            || entry.name.to_lowercase().contains(&query)
                            || entry.app_id.to_string().contains(&query))
                })
                .take(200)
                .collect();
            ui.label(
                RichText::new(format!(
                    "{} result{}",
                    matches.len(),
                    if matches.len() == 1 { "" } else { "s" }
                ))
                .size(11.0)
                .color(MUTED),
            );
            ui.add_space(8.0);
            let mut open = None;
            if matches.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("No games match your search.").color(MUTED));
                ui.add_space(6.0);
            }
            // Same row structure as the Featured list (fixed slot + explicit gap below) for identical
            // card spacing.
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            for entry in &matches {
                let selected = self.selected_app == Some(entry.app_id);
                let mut clicked = false;
                list_row_slot(ui, width, |ui| {
                    clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
                });
                ui.add_space(LIST_ROW_GAP);
                if clicked {
                    open = Some(entry.app_id);
                }
            }
            open
        };
        if let Some(app_id) = open {
            self.open_details(app_id);
        }
    }

    /// The Library page: unlike the Store (which lists every supported game), this shows only the
    /// user's own collection — games Steam has installed plus games activated through Drydock — each as
    /// a capsule with a Play button. A game activated outside Steam (no manifest) can be pointed at
    /// its own `.exe` so it launches from here too, just like a Steam-installed game.
    fn library_page(&mut self, ui: &mut egui::Ui) {
        // When a folder has been chosen for "Add game to Drydock", the page becomes the add-mode picker.
        if self.add_game_folder.is_some() {
            self.render_add_game_panel(ui);
            return;
        }
        // Installed (real) games first, then games activated in Drydock that Steam hasn't installed.
        let mut entries: Vec<LibraryEntry> = Vec::new();
        let mut seen: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for manifest in &self.manifests {
            if is_real_game(manifest.app_id, &manifest.name) && seen.insert(manifest.app_id) {
                entries.push(LibraryEntry {
                    app_id: manifest.app_id,
                    name: manifest.name.clone(),
                    installed: true,
                    launch_path: self.settings.launch_paths.get(&manifest.app_id).cloned(),
                    source: LibrarySource::SteamInstalled,
                });
            }
        }
        // Games Drydock downloaded through its own depot engine into the games folder (not the Steam
        // library) — shown alongside the Steam-detected ones.
        for (app_id, game) in &self.settings.installed_games {
            if seen.insert(*app_id) {
                entries.push(LibraryEntry {
                    app_id: *app_id,
                    name: game.name.clone(),
                    installed: true,
                    launch_path: self.settings.launch_paths.get(app_id).cloned(),
                    source: LibrarySource::DrydockInstalled,
                });
            }
        }
        for app_id in self.settings.added_apps.keys().copied() {
            if !seen.insert(app_id) {
                continue;
            }
            let name = self
                .catalog
                .iter()
                .find(|entry| entry.app_id == app_id)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| format!("App {app_id}"));
            if !is_real_game(app_id, &name) {
                continue;
            }
            entries.push(LibraryEntry {
                app_id,
                name,
                installed: false,
                launch_path: self.settings.launch_paths.get(&app_id).cloned(),
                source: LibrarySource::Available,
            });
        }
        entries.sort_by_key(|entry| entry.name.to_lowercase());

        // Set by the "Add game to Drydock" button; the folder picker runs after the borrow of `entries`
        // and the UI closures ends (a native dialog can't open mid-layout).
        let mut add_game_requested = false;

        ui.add_space(20.0);
        page_heading(ui, "Library");
        ui.add_space(4.0);
        ui.add(egui::Label::new(
            RichText::new("Your installed and activated games — launch any of them straight from Drydock.")
                .size(12.5)
                .color(MUTED),
        ));
        ui.add_space(16.0);

        if entries.is_empty() {
            panel(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("No games in your library yet.")
                        .size(13.5)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Install a game through Steam or activate one in Drydock and it will show up here.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(primary_button("BROWSE THE STORE")).clicked() {
                        self.page = Page::Home;
                    }
                    if ui.add(ghost_button("＋  ADD GAME TO DRYDOCK")).clicked() {
                        add_game_requested = true;
                    }
                });
            });
            if add_game_requested {
                self.begin_add_game();
            }
            return;
        }

        // Keep the highlighted game valid: default to the first, and drop a stale selection.
        if self
            .library_selected
            .is_none_or(|id| !entries.iter().any(|entry| entry.app_id == id))
        {
            self.library_selected = entries.first().map(|entry| entry.app_id);
        }

        // A fixed-height body so the rail and the overview each get their own scroll — a nested
        // scroll area needs a definite height, which the growing outer page scroll doesn't provide.
        let top_used = TOP_NAV_HEIGHT + STATUS_BAR_HEIGHT + 18.0;
        let screen_h = ui
            .ctx()
            .input(|input| input.raw.screen_rect.map(|rect| rect.height()))
            .unwrap_or(900.0);
        let body_h = (screen_h - top_used - 150.0).max(340.0);
        let rail_w = 300.0_f32.min(ui.available_width() * 0.42);

        let mut action: Option<LibraryAction> = None;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), body_h),
            Layout::left_to_right(Align::Min),
            |ui| {
                // Left rail: every game, activated-only ones dimmed grey (Steam-style).
                ui.allocate_ui_with_layout(Vec2::new(rail_w, body_h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(rail_w);
                    egui::Frame::new()
                        .fill(SIDEBAR_FILL)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(12)
                        .inner_margin(6)
                        .show(ui, |ui| {
                            ui.set_min_height(body_h - 14.0);
                            let add_w = ui.available_width();
                            if ui
                                .add(ghost_button("＋  ADD GAME TO DRYDOCK").min_size(Vec2::new(add_w, 34.0)))
                                .clicked()
                            {
                                add_game_requested = true;
                            }
                            ui.add_space(6.0);
                            egui::ScrollArea::vertical()
                                .id_salt("library_rail")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let selected = self.library_selected;
                                    // Steam-style collapsible groups, all open by default.
                                    for (title, source) in [
                                        ("Installed in Steam", LibrarySource::SteamInstalled),
                                        ("Installed in Drydock", LibrarySource::DrydockInstalled),
                                        ("Available", LibrarySource::Available),
                                    ] {
                                        if let Some(id) =
                                            library_rail_group(ui, title, source, &entries, selected)
                                        {
                                            action = Some(LibraryAction::Select(id));
                                        }
                                    }
                                });
                        });
                });
                ui.add_space(20.0);
                // Right overview of the highlighted game. It needs its own top-down layout, or the
                // banner + info would flow horizontally inside this left-to-right row.
                if let Some(entry) = self
                    .library_selected
                    .and_then(|id| entries.iter().find(|entry| entry.app_id == id))
                {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), body_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            if let Some(overview_action) = library_overview(ui, entry, body_h) {
                                action = Some(overview_action);
                            }
                        },
                    );
                }
            },
        );

        match action {
            Some(LibraryAction::Select(app_id)) => self.library_selected = Some(app_id),
            Some(LibraryAction::Details(app_id)) => self.open_details(app_id),
            Some(LibraryAction::Launch(app_id)) => self.launch_library_game(app_id),
            Some(LibraryAction::SetExe(app_id)) => self.set_library_launch_path(app_id),
            Some(LibraryAction::InstallSteam(app_id)) => self.install_steam_game(app_id),
            Some(LibraryAction::UninstallSteam(app_id)) => self.uninstall_steam_game(app_id),
            Some(LibraryAction::UpdateLua(app_id)) => self.add_app_to_steam(app_id),
            Some(LibraryAction::RemoveLua(app_id)) => self.remove_lua_confirmed(app_id),
            Some(LibraryAction::VerifyDrydock(app_id)) => {
                let name = self.app_display_name(app_id);
                self.start_verify(app_id, name);
            }
            Some(LibraryAction::UpdateDrydock(app_id)) => {
                let name = self.app_display_name(app_id);
                self.enqueue_download(app_id, name);
                self.status = format!("Checking {} for updates…", self.app_display_name(app_id));
                self.status_error = false;
            }
            Some(LibraryAction::CrackDrydock(app_id)) => self.crack_drydock_game(app_id),
            Some(LibraryAction::UninstallDrydock(app_id)) => self.uninstall_drydock_game(app_id),
            None => {}
        }

        if add_game_requested {
            self.begin_add_game();
        }
    }

    /// Launches a library game: a stored `.exe` (for games activated outside Steam) wins; otherwise
    /// Steam is asked to run the App ID.
    fn launch_library_game(&mut self, app_id: u32) {
        if let Some(path) = self.settings.launch_paths.get(&app_id).cloned() {
            let exe = PathBuf::from(&path);
            let mut command = Command::new(&exe);
            if let Some(parent) = exe.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                command.current_dir(parent);
            }
            match command.spawn() {
                Ok(_) => {
                    self.status = "Launching the game".into();
                    self.status_error = false;
                }
                Err(error) => {
                    self.status = format!("The game could not be launched: {error}");
                    self.status_error = true;
                }
            }
            return;
        }
        match open_steam_uri(app_id, SteamUriAction::Run) {
            Ok(()) => {
                self.status = "Asking Steam to launch the game".into();
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// Picks the launch `.exe` for a game activated outside Steam, and remembers it so the Play
    /// button starts it directly next time.
    fn set_library_launch_path(&mut self, app_id: u32) {
        let mut dialog = rfd::FileDialog::new().set_title("Select the game's .exe");
        if let Some(current) = self
            .settings
            .launch_paths
            .get(&app_id)
            .map(PathBuf::from)
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .filter(|parent| parent.is_dir())
        {
            dialog = dialog.set_directory(current);
        }
        #[cfg(windows)]
        {
            dialog = dialog.add_filter("Executable", &["exe"]);
        }
        if let Some(file) = dialog.pick_file() {
            self.settings
                .launch_paths
                .insert(app_id, file.display().to_string());
            self.status_error = self.persist_settings().is_err();
            if self.status_error {
                self.status = "The launch path could not be saved".into();
            } else {
                self.status = "Launch path saved".into();
            }
        }
    }

    /// Asks Steam to install a game whose unlock Lua is already in place (`steam://install`).
    fn install_steam_game(&mut self, app_id: u32) {
        match open_steam_uri(app_id, SteamUriAction::Install) {
            Ok(()) => {
                self.status = "Asking Steam to install the game".into();
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// Asks Steam to uninstall a Steam-installed game (`steam://uninstall`); Steam shows its own
    /// confirmation, so no extra dialog here.
    fn uninstall_steam_game(&mut self, app_id: u32) {
        match open_steam_uri(app_id, SteamUriAction::Uninstall) {
            Ok(()) => {
                self.status = "Asking Steam to uninstall the game".into();
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// Removes a game's unlock Lua from Steam after a confirmation prompt.
    fn remove_lua_confirmed(&mut self, app_id: u32) {
        let name = self.app_display_name(app_id);
        let confirmed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Remove unlock Lua")
            .set_description(format!(
                "Delete the unlock Lua for \"{name}\" from Steam? You can add it again any time."
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed == rfd::MessageDialogResult::Yes {
            self.remove_app_from_steam(app_id);
        }
    }

    /// Deletes a Drydock-downloaded game's install folder (after confirmation) and forgets it.
    fn uninstall_drydock_game(&mut self, app_id: u32) {
        let Some(game) = self.settings.installed_games.get(&app_id).cloned() else {
            return;
        };
        let name = if game.name.is_empty() {
            self.app_display_name(app_id)
        } else {
            game.name.clone()
        };
        let confirmed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Uninstall game")
            .set_description(format!(
                "Delete \"{name}\" and all of its downloaded files from:\n{}\n\nThis cannot be undone.",
                game.install_dir
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed != rfd::MessageDialogResult::Yes {
            return;
        }
        let dir = PathBuf::from(&game.install_dir);
        if dir.is_dir()
            && let Err(error) = std::fs::remove_dir_all(&dir)
        {
            self.status = format!("Could not delete the game folder: {error}");
            self.status_error = true;
            return;
        }
        self.settings.installed_games.remove(&app_id);
        self.settings
            .download_queue
            .retain(|queued| queued.app_id != app_id);
        self.status_error = self.persist_settings().is_err();
        self.status = if self.status_error {
            format!("\"{name}\" was deleted, but the change could not be saved")
        } else {
            format!("\"{name}\" was uninstalled")
        };
    }

    /// Runs the emu crack flow for a Drydock-downloaded game, deploying it straight into the game's
    /// install folder. Reuses the same generator as the Tools cracker (and the Tools panel's arch /
    /// loader / REFramework choices), just with the output fixed to this game's folder.
    fn crack_drydock_game(&mut self, app_id: u32) {
        let Some(game) = self.settings.installed_games.get(&app_id).cloned() else {
            return;
        };
        if self.emu_receiver.is_some() {
            self.status = "A crack job is already running.".into();
            self.status_error = true;
            return;
        }
        let root = PathBuf::from(&game.install_dir);
        if !root.is_dir() {
            self.status = "The game's install folder no longer exists.".into();
            self.status_error = true;
            return;
        }
        let skeleton = {
            let path = self.settings.emu_skeleton_path.trim();
            (!path.is_empty()).then(|| PathBuf::from(path))
        };
        let loader = if self.emu_loader_winmm {
            "winmm.dll"
        } else {
            "version.dll"
        };
        let arch = self.emu_arch;
        let reframework = self.emu_reframework;
        let cache_dir = self.paths.cache_dir();
        let name = if game.name.is_empty() {
            self.app_display_name(app_id)
        } else {
            game.name.clone()
        };
        let (sender, receiver) = mpsc::channel();
        self.emu_receiver = Some(receiver);
        self.status = format!("Cracking {name} — resolving config and emu files…");
        self.status_error = false;
        std::thread::spawn(move || {
            let _ = sender.send(build_emu_crack(
                app_id,
                &EmuOutput::Deploy(root),
                arch,
                loader,
                reframework,
                skeleton.as_deref(),
                &cache_dir,
            ));
        });
    }

    /// Starts the "Add game to Drydock" flow: pick the game's folder. Picking one puts the Library into
    /// add-mode, where the user then says which game the folder is.
    fn begin_add_game(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Select the game's folder")
            .pick_folder()
        {
            self.add_game_folder = Some(folder);
            self.add_game_search.clear();
        }
    }

    /// Detects a chosen folder in the background: it fetches the game's launch executables from Steam,
    /// resolves the real game root inside the folder, and finds the launch `.exe` — exactly as the
    /// activation folder check does — so the game can be added to the Drydock library and played.
    fn start_add_game_detect(&mut self, app_id: u32, folder: PathBuf) {
        if self.add_game_receiver.is_some() {
            return;
        }
        let name = self.app_display_name(app_id);
        let (sender, receiver) = mpsc::channel();
        self.add_game_receiver = Some(receiver);
        self.busy_label = Some(format!("Detecting {name}…"));
        std::thread::spawn(move || {
            let result = (|| -> Result<AddGameOutcome, String> {
                if !folder.is_dir() {
                    return Err("Select the game's folder first.".to_owned());
                }
                let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
                if executables.is_empty() {
                    return Err("Steam lists no launch executable for this game.".to_owned());
                }
                let root = resolve_game_root(&folder, &executables).ok_or_else(|| {
                    "These files don't look like the selected game. Pick the correct game folder.".to_owned()
                })?;
                // resolve_game_root guarantees at least one executable exists under the root; use the
                // first one that does as the Play launcher.
                let exe = executables
                    .iter()
                    .map(|relative| root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
                    .find(|path| path.is_file())
                    .ok_or_else(|| "The launch executable could not be found in the folder.".to_owned())?;
                Ok(AddGameOutcome {
                    app_id,
                    name,
                    root,
                    exe,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Applies a finished "Add game to Drydock" detection: records the game under the Drydock library and
    /// remembers its launch `.exe` so Play works.
    fn poll_add_game(&mut self) {
        let Some(receiver) = self.add_game_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.add_game_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(outcome) => {
                        self.settings.installed_games.insert(
                            outcome.app_id,
                            drydock_core::InstalledGame {
                                name: outcome.name.clone(),
                                install_dir: outcome.root.display().to_string(),
                            },
                        );
                        self.settings
                            .launch_paths
                            .insert(outcome.app_id, outcome.exe.display().to_string());
                        self.status_error = self.persist_settings().is_err();
                        self.status = if self.status_error {
                            format!(
                                "\"{}\" was added, but the change could not be saved",
                                outcome.name
                            )
                        } else {
                            format!("\"{}\" was added to your Drydock library", outcome.name)
                        };
                        self.add_game_folder = None;
                        self.add_game_search.clear();
                        self.library_selected = Some(outcome.app_id);
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.add_game_receiver = None,
        }
    }

    /// After a depot download finishes, detects the game's install folder and launch `.exe` in the
    /// background (the same install root the depot engine wrote to, plus Steam's executable list) so
    /// the game registers itself under "Installed in Drydock" with a working Play button.
    fn start_download_install_detect(&mut self, app_id: u32, name: String) {
        if self.download_install_receiver.is_some() {
            return;
        }
        let steam_root = self.steam.root.clone();
        let installed_dir = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(SteamManifest::install_dir);
        let (sender, receiver) = mpsc::channel();
        self.download_install_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| -> Result<AddGameOutcome, String> {
                // The same install root the depot engine wrote to (mirrors run_depot_job).
                let install_root = match installed_dir {
                    Some(dir) => dir,
                    None => {
                        let root = steam_root.ok_or_else(|| "Steam folder not found.".to_owned())?;
                        let installdir = fetch_install_dir(app_id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| {
                                "Steam did not report an install folder for this game.".to_owned()
                            })?;
                        root.join("steamapps").join("common").join(installdir)
                    }
                };
                let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
                // The depot writes straight into the install root; resolve a nested root only if the
                // launch exe lives in a subfolder.
                let root = resolve_game_root(&install_root, &executables).unwrap_or(install_root);
                let exe = executables
                    .iter()
                    .map(|relative| root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
                    .find(|path| path.is_file())
                    .unwrap_or_default();
                Ok(AddGameOutcome {
                    app_id,
                    name,
                    root,
                    exe,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Registers a finished depot download in the Drydock library (quietly — the download's own success
    /// message stays on screen). A detection failure is ignored: the files are still on disk and the
    /// game can be added manually.
    fn poll_download_install(&mut self) {
        let Some(receiver) = self.download_install_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.download_install_receiver = None;
                if let Ok(outcome) = result {
                    self.settings.installed_games.insert(
                        outcome.app_id,
                        drydock_core::InstalledGame {
                            name: outcome.name.clone(),
                            install_dir: outcome.root.display().to_string(),
                        },
                    );
                    if !outcome.exe.as_os_str().is_empty() {
                        self.settings
                            .launch_paths
                            .insert(outcome.app_id, outcome.exe.display().to_string());
                    }
                    let _ = self.persist_settings();
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.download_install_receiver = None,
        }
    }

    /// The add-mode panel shown in place of the Library while a folder is chosen: the picked folder
    /// plus a game picker to say which game it is (then background detection runs).
    fn render_add_game_panel(&mut self, ui: &mut egui::Ui) {
        let Some(folder) = self.add_game_folder.clone() else {
            return;
        };
        ui.add_space(20.0);
        page_heading(ui, "Add a game to Drydock");
        ui.add_space(4.0);
        ui.add(egui::Label::new(
            RichText::new("Point Drydock at a game you already have on disk — it detects the folder and the launch .exe so you can play it from here.")
                .size(12.5)
                .color(MUTED),
        ));
        ui.add_space(16.0);

        let busy = self.add_game_receiver.is_some();
        let mut picked: Option<u32> = None;
        let mut cancel = false;
        panel(ui, |ui| {
            ui.label(RichText::new("GAME FOLDER").size(9.0).strong().color(ACCENT));
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(RichText::new(folder.display().to_string()).size(12.5).color(TEXT)).wrap(),
            );
            ui.add_space(16.0);
            ui.label(
                RichText::new("WHICH GAME IS THIS?")
                    .size(9.0)
                    .strong()
                    .color(ACCENT),
            );
            ui.add_space(6.0);
            if busy {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Detecting the game folder…")
                            .size(12.5)
                            .color(MUTED),
                    );
                });
            } else {
                let width = ui.available_width();
                picked = game_search_box(
                    ui,
                    "add_game_pick",
                    &mut self.add_game_search,
                    None,
                    &self.catalog,
                    &self.header_resolver,
                    width,
                    true,
                    |_| true,
                );
            }
            ui.add_space(16.0);
            if ui
                .add(ghost_button("CANCEL").min_size(Vec2::new(120.0, 38.0)))
                .clicked()
            {
                cancel = true;
            }
        });

        if cancel {
            self.add_game_folder = None;
            self.add_game_search.clear();
        } else if let Some(app_id) = picked {
            self.start_add_game_detect(app_id, folder);
        }
    }

    /// The Home filter bar: a Repacks dropdown (Any / All repacks / each repacker) and a Fixes
    /// dropdown (Any / Online / Denuvo), styled to match the cards, plus a Clear link when active.
    fn home_filter_row(&mut self, ui: &mut egui::Ui) {
        let active = self.repack_filter != RepackFilter::Any || self.fix_filter != FixFilter::Any;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;

            filter_dropdown(
                ui,
                "home_repack_filter",
                "REPACKS",
                &self.repack_filter.label(),
                168.0,
                |ui| {
                    ui.selectable_value(&mut self.repack_filter, RepackFilter::Any, "Any");
                    ui.selectable_value(&mut self.repack_filter, RepackFilter::AnyRepack, "All repacks");
                    for name in &self.available_repackers {
                        let selected = self.repack_filter == RepackFilter::Repacker(name.clone());
                        if ui.selectable_label(selected, name).clicked() {
                            self.repack_filter = RepackFilter::Repacker(name.clone());
                        }
                    }
                },
            );

            filter_dropdown(
                ui,
                "home_fix_filter",
                "FIXES",
                self.fix_filter.label(),
                148.0,
                |ui| {
                    ui.selectable_value(&mut self.fix_filter, FixFilter::Any, "Any");
                    ui.selectable_value(&mut self.fix_filter, FixFilter::Denuvo, "Denuvo");
                },
            );

            if active {
                // Align the Clear link with the dropdown boxes (a label sits above those).
                ui.vertical(|ui| {
                    ui.add_space(17.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("✕  Clear").size(11.0).color(ACCENT))
                                .frame(false),
                        )
                        .clicked()
                    {
                        self.repack_filter = RepackFilter::Any;
                        self.fix_filter = FixFilter::Any;
                    }
                });
            }
        });
    }

    /// Builds the Apply-Fix panel for the details page when the app has a build-locked Denuvo fix
    /// (from the GitHub MFB repo). The DepotBox "online fix" API path has been removed.
    fn details_fix_panel(&self, app_id: u32) -> Option<FixPanelState> {
        let fix = self.fix_for(app_id)?;
        let denuvo = fix.denuvo.as_ref()?;
        // The Denuvo fix's applied status needs the Steam folder to inspect the plug-in Lua.
        let status = self
            .steam
            .root
            .as_deref()
            .map_or(FixStatus::NotApplied, |root| fix_status(root, denuvo));
        Some(FixPanelState {
            busy: self.background_action.is_some(),
            installed: self.manifests.iter().any(|manifest| manifest.app_id == app_id),
            denuvo: Some(status),
        })
    }

    /// Builds the repack Download-button panel for the details page when the app has repack sources.
    fn details_repack_panel(&self, app_id: Option<u32>) -> Option<RepackPanelState> {
        let repack = self.repack_for(app_id?)?;
        Some(RepackPanelState {
            repackers: repack
                .sources
                .iter()
                .map(|source| source.repacker.clone())
                .collect(),
        })
    }

    fn details_state(&self, _manifest: &Option<SteamManifest>) -> DetailsState {
        DetailsState {
            is_added: self
                .details_app_id
                .is_some_and(|app_id| self.settings.added_apps.contains_key(&app_id)),
            service_current: matches!(
                self.service_status.as_ref().map(|status| status.state),
                Some(SteamServiceState::Current)
            ),
            busy: self.service_receiver.is_some(),
        }
    }

    /// Dispatches a details-page button click to the matching Steam or unlock operation.
    fn perform_details_action(&mut self, app_id: u32, name: &str, action: DetailsAction) {
        match action {
            DetailsAction::None => {}
            DetailsAction::AddToSteam => self.add_app_to_steam(app_id),
            DetailsAction::AddCracked => self.add_cracked_to_steam(app_id),
            DetailsAction::RemoveFromSteam => self.remove_app_from_steam(app_id),
            DetailsAction::InstallService => self.install_steam_service(),
            DetailsAction::ApplyDenuvoFix => self.apply_denuvo_fix_for(app_id),
            DetailsAction::Download(index) => self.open_repack_source(app_id, index),
            DetailsAction::Activate => self.go_to_activation(app_id),
            DetailsAction::DepotDownload => self.enqueue_download(app_id, name.to_owned()),
            DetailsAction::DepotVerify => self.start_verify(app_id, name.to_owned()),
        }
    }

    /// Opens the chosen repack source's link in the user's browser (http(s) only).
    fn open_repack_source(&mut self, app_id: u32, index: usize) {
        let Some(source) = self
            .repack_for(app_id)
            .and_then(|repack| repack.sources.get(index))
        else {
            return;
        };
        let repacker = source.repacker.clone();
        let link = source.link.clone();
        match open_link(&link) {
            Ok(()) => {
                self.status = format!("Opening {repacker} in your browser");
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    fn details_page(&mut self, ui: &mut egui::Ui) {
        if back_button(ui, "Return to search").clicked() {
            self.page = Page::Home;
            return;
        }
        ui.add_space(18.0);

        let manifest = self
            .details_app_id
            .and_then(|app_id| self.manifests.iter().find(|app| app.app_id == app_id))
            .cloned();
        let fallback_name = self
            .details_app_id
            .and_then(|app_id| self.catalog.iter().find(|app| app.app_id == app_id))
            .map(|app| app.name.clone())
            .or_else(|| manifest.as_ref().map(|app| app.name.clone()))
            .unwrap_or_else(|| "GAME DETAILS".to_owned());

        if self.store_loading {
            page_heading(ui, &fallback_name);
            ui.add_space(36.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading details and artwork").color(MUTED));
            });
            return;
        }

        let state = self.details_state(&manifest);
        let app_id = self.details_app_id;

        let Some(details) = self.store_details.as_ref() else {
            page_heading(ui, &fallback_name);
            ui.add_space(20.0);
            let fix = app_id.and_then(|id| self.details_fix_panel(id));
            let repack = self.details_repack_panel(app_id);
            let mut action = DetailsAction::None;
            panel(ui, |ui| {
                ui.label(RichText::new("Store information is currently unavailable.").color(MUTED));
                if let Some(app) = &manifest {
                    ui.label(RichText::new(format!("APP {}", app.app_id)).color(ACCENT));
                    ui.label(RichText::new(app.install_dir().display().to_string()).color(MUTED));
                } else if let Some(id) = app_id {
                    ui.label(RichText::new(format!("APP {id}")).color(ACCENT));
                    ui.label(RichText::new("Not installed through Steam").color(MUTED));
                }
                if let Some(id) = app_id {
                    let activation_required = self.needs_activation(id) == Some(true);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        action = steam_button_row(
                            ui,
                            state,
                            DetailsPanels {
                                fix: fix.as_ref(),
                                repack: repack.as_ref(),
                            },
                            activation_required,
                        );
                    });
                }
            });
            if let Some(id) = app_id {
                self.perform_details_action(id, &fallback_name, action);
            }
            return;
        };

        // Centre the details in a comfortable max-width column: symmetric margins on wide
        // screens read as intentional, and text lines stay readable instead of stretching
        // edge to edge.
        // The page is already centred and width-capped by the shell, so render at full column width.
        let content_width = ui.available_width();
        let shot = self.screenshot_index;
        // Activation is offered only for real Denuvo games: the Steam "Denuvo Watch" curator, or
        // Steam's own store DRM notice for this app (`uses_denuvo`). The MFB fix list never factors
        // in here — it drives only the separate "Add cracked version" button.
        let activation_required = self.needs_activation(details.app_id) == Some(true) || details.uses_denuvo;
        // The Apply Fix buttons only appear for details opened from the Fixes tab, and only when
        // a fix exists for this app (one button per variant).
        let fix = self.details_fix_panel(details.app_id);
        let repack = self.details_repack_panel(Some(details.app_id));
        let depot = DepotButtons {
            // Depot availability isn't cheaply probeable, so offer Download for any real game and
            // report "no depot data" at download time if the package turns out to be missing.
            downloadable: is_real_game(details.app_id, &details.name),
            installed: self
                .manifests
                .iter()
                .any(|manifest| manifest.app_id == details.app_id),
            busy: self
                .download_job
                .as_ref()
                .is_some_and(|job| job.finished.is_none()),
        };
        let (action, new_shot) = details_body(
            ui,
            details,
            state,
            content_width,
            shot,
            activation_required,
            DetailsPanels {
                fix: fix.as_ref(),
                repack: repack.as_ref(),
            },
            depot,
        );
        self.screenshot_index = new_shot;
        let name = details.name.clone();
        self.perform_details_action(details.app_id, &name, action);
    }

    fn activation_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "ACTIVATION");
        ui.add_space(18.0);
        content_column(ui, CONTENT_WIDTH, |ui| {
            self.activation_switcher(ui);
            ui.add_space(16.0);
            match self.activation_provider {
                ActivationProvider::Steam => self.activation_steam_body(ui),
                ActivationProvider::Ubisoft => self.activation_ubisoft_body(ui),
                ActivationProvider::Ea => activation_ea_body(ui),
            }
        });
    }

    /// The STEAM / UBISOFT / EA segmented switcher at the top of the Activation page.
    fn activation_switcher(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (provider, label) in [
                (ActivationProvider::Steam, "STEAM"),
                (ActivationProvider::Ubisoft, "UBISOFT"),
                (ActivationProvider::Ea, "EA"),
            ] {
                if activation_segment(ui, label, self.activation_provider == provider) {
                    self.activation_provider = provider;
                }
            }
        });
    }

    /// The Steam activation flow (unchanged): pick a game and folder, generate a machine-bound
    /// request code, then paste the bot's response code to verify and install the entitlement.
    fn activation_steam_body(&mut self, ui: &mut egui::Ui) {
        panel(ui, |ui| {
            section_label(ui, "SELECT GAME");
            ui.add_space(8.0);
            let selector_width = ui.available_width();

            // Dynamic search: type a name or App ID, then pick from the fixed-height result list.
            if let Some(app_id) = game_search_box(
                ui,
                "activation_search",
                &mut self.activation_search,
                self.selected_app,
                &self.catalog,
                &self.header_resolver,
                selector_width,
                false,
                |_| true,
            ) {
                self.select_activation_game(app_id);
            }

            if let Some(app_id) = self.selected_app {
                ui.add_space(16.0);
                section_label(ui, "GAME FOLDER");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let field_width = (selector_width - 132.0).max(200.0);
                    ui.add_sized(
                        [field_width, 40.0],
                        egui::TextEdit::singleline(&mut self.activation_path)
                            .hint_text("…\\steamapps\\common\\Game  (or the repack folder)")
                            .margin(egui::Margin::symmetric(12, 10)),
                    );
                    if ui
                        .add_sized([120.0, 40.0], ghost_button("CHOOSE FOLDER"))
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        self.activation_path = folder.display().to_string();
                        self.activation_root = None;
                    }
                });

                ui.add_space(20.0);
                let busy = self.activation_check_receiver.is_some()
                    || self.activation_remove_receiver.is_some()
                    || self.activation_receiver.is_some();
                ui.horizontal(|ui| {
                    let can_generate = !self.activation_path.trim().is_empty() && !busy;
                    if ui
                        .add_enabled(can_generate, primary_button("GENERATE ACTIVATION CODE"))
                        .on_hover_text("Verify the game folder, then create a machine-bound request code")
                        .clicked()
                    {
                        self.start_activation_check(app_id, PathBuf::from(self.activation_path.trim()));
                    }
                });
            } else if self.catalog.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the game catalog…").size(10.5).color(MUTED));
            }
            if !self.activation_request_code.is_empty() {
                let short_request = is_short_activation_code(&self.activation_request_code);
                ui.add_space(18.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(18, 21, 24))
                    .corner_radius(12)
                    .inner_margin(18)
                    .show(ui, |ui| {
                        section_label(
                            ui,
                            if short_request {
                                "REQUEST CODE"
                            } else {
                                "COMPLETE FALLBACK REQUEST"
                            },
                        );
                        if short_request {
                            ui.label(
                                RichText::new(&self.activation_request_code)
                                    .size(22.0)
                                    .strong()
                                    .color(TEXT),
                            );
                        } else {
                            ui.add_sized(
                                [ui.available_width(), 78.0],
                                egui::TextEdit::multiline(&mut self.activation_request_code)
                                    .font(egui::TextStyle::Monospace)
                                    .margin(egui::Margin::symmetric(12, 10))
                                    .interactive(false),
                            );
                        }
                        if ui
                            .add(ghost_button("COPY CODE"))
                            .on_hover_text("Copy the activation request code to the clipboard")
                            .clicked()
                        {
                            ui.ctx().copy_text(self.activation_request_code.clone());
                            self.status = "Activation code copied".into();
                            self.status_error = false;
                        }
                        ui.add_space(18.0);
                        section_label(ui, "RESPONSE CODE FROM THE BOT");
                        ui.add_space(7.0);
                        response_code_fields(ui, &mut self.response_code);
                        ui.add_space(12.0);
                        let response_is_complete = self.response_code.iter().all(|part| {
                            part.len() == 1 && part.chars().all(|value| value.is_ascii_alphanumeric())
                        });
                        if ui
                            .add_enabled(
                                response_is_complete && self.activation_verify_receiver.is_none(),
                                primary_button("ACTIVATE"),
                            )
                            .on_hover_text("Verify and install the activation response")
                            .clicked()
                            && let Some(app_id) = self.selected_app
                        {
                            self.verify_activation_response(app_id);
                        }
                        if let Some(entitlement) = &self.verified_entitlement {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "SIGNED ENTITLEMENT VERIFIED · {} protected bytes · valid until {}",
                                    entitlement.payload_bytes, entitlement.expires_utc
                                ))
                                .size(9.5)
                                .strong()
                                .color(ACCENT_SOFT),
                            );
                        }
                    });
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if short_request {
                        "Send the short request code in your Steam ticket. It expires after 30 minutes."
                    } else {
                        "The upload service could not be reached. Copy the complete fallback request into the ticket."
                    })
                    .size(10.5)
                    .color(ACCENT),
                );
            }
        });
    }

    /// The Ubisoft activation flow: pick a game + folder, PREPARE (adds magicfiles, launches the
    /// game once, captures token_req.txt into a machine-bound activation code), then paste the bot's
    /// response code to install token.ini next to the game exe.
    fn activation_ubisoft_body(&mut self, ui: &mut egui::Ui) {
        panel(ui, |ui| {
            section_label(ui, "SELECT GAME");
            ui.add_space(8.0);
            let selector_width = ui.available_width();
            if let Some(app_id) = game_search_box(
                ui,
                "ubisoft_search",
                &mut self.activation_search,
                self.selected_app,
                &self.catalog,
                &self.header_resolver,
                selector_width,
                false,
                |_| true,
            ) {
                self.select_activation_game(app_id);
            }

            if let Some(app_id) = self.selected_app {
                ui.add_space(16.0);
                section_label(ui, "GAME FOLDER");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let field_width = (selector_width - 132.0).max(200.0);
                    ui.add_sized(
                        [field_width, 40.0],
                        egui::TextEdit::singleline(&mut self.activation_path)
                            .hint_text("…\\steamapps\\common\\Game")
                            .margin(egui::Margin::symmetric(12, 10)),
                    );
                    if ui
                        .add_sized([120.0, 40.0], ghost_button("CHOOSE FOLDER"))
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        self.activation_path = folder.display().to_string();
                        self.ubisoft_activation_code.clear();
                    }
                });

                ui.add_space(14.0);
                ui.label(
                    RichText::new(
                        "Drydock adds the Ubisoft magicfiles, launches the game once, and reads the \
                         token_req.txt it generates. Close the game once the token request appears.",
                    )
                    .size(10.5)
                    .color(MUTED),
                );

                ui.add_space(12.0);
                let busy = self.ubisoft_prepare_receiver.is_some();
                let can_prepare = !self.activation_path.trim().is_empty() && !busy;
                if ui
                    .add_enabled(can_prepare, primary_button("PREPARE & LAUNCH GAME"))
                    .on_hover_text("Install magicfiles, launch the game, and capture its token request")
                    .clicked()
                {
                    self.start_ubisoft_prepare(app_id, PathBuf::from(self.activation_path.trim()));
                }
            } else if self.catalog.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the game catalog…").size(10.5).color(MUTED));
            }

            if !self.ubisoft_activation_code.is_empty() {
                ui.add_space(18.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(18, 21, 24))
                    .corner_radius(12)
                    .inner_margin(18)
                    .show(ui, |ui| {
                        section_label(ui, "ACTIVATION CODE");
                        ui.label(
                            RichText::new(&self.ubisoft_activation_code)
                                .size(22.0)
                                .strong()
                                .color(TEXT),
                        );
                        if ui
                            .add(ghost_button("COPY CODE"))
                            .on_hover_text("Copy the Ubisoft activation code to the clipboard")
                            .clicked()
                        {
                            ui.ctx().copy_text(self.ubisoft_activation_code.clone());
                            self.status = "Activation code copied".into();
                            self.status_error = false;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Send this code in your Ubisoft ticket. It expires after 30 minutes.",
                            )
                            .size(10.5)
                            .color(ACCENT),
                        );

                        ui.add_space(18.0);
                        section_label(ui, "RESPONSE CODE FROM THE BOT");
                        ui.add_space(7.0);
                        response_code_fields(ui, &mut self.response_code);
                        ui.add_space(12.0);
                        let response_is_complete = self.response_code.iter().all(|part| {
                            part.len() == 1 && part.chars().all(|value| value.is_ascii_alphanumeric())
                        });
                        if ui
                            .add_enabled(
                                response_is_complete && self.activation_verify_receiver.is_none(),
                                primary_button("ACTIVATE"),
                            )
                            .on_hover_text("Verify the response and install token.ini next to the game")
                            .clicked()
                            && let Some(app_id) = self.selected_app
                        {
                            self.verify_ubisoft_response(app_id);
                        }
                        if let Some(entitlement) = &self.verified_entitlement {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "TOKEN INSTALLED · {} bytes · valid until {}",
                                    entitlement.payload_bytes, entitlement.expires_utc
                                ))
                                .size(9.5)
                                .strong()
                                .color(ACCENT_SOFT),
                            );
                        }
                    });
            }
        });
    }

    /// Runs the whole Ubisoft prepare sequence on a background thread (folder verify, magicfiles
    /// download + install, first launch, token_req.txt capture, activation-code generation).
    fn start_ubisoft_prepare(&mut self, app_id: u32, chosen: PathBuf) {
        if self.ubisoft_prepare_receiver.is_some() || self.activation_verify_receiver.is_some() {
            return;
        }
        self.ubisoft_activation_code.clear();
        self.ubisoft_exe_dir = None;
        self.verified_entitlement = None;
        self.response_code = std::array::from_fn(|_| String::new());
        self.status = "Preparing Ubisoft activation…".into();
        self.status_error = false;
        self.busy_label = Some("Adding magicfiles and launching the game…".into());
        let settings_directory = self.paths.settings_dir();
        let (sender, receiver) = mpsc::channel();
        self.ubisoft_prepare_receiver = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(prepare_ubisoft(app_id, &chosen, &settings_directory));
        });
    }

    fn poll_ubisoft_prepare(&mut self) {
        let Some(receiver) = self.ubisoft_prepare_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.ubisoft_prepare_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(prepared) => {
                        self.ubisoft_activation_code = prepared.activation_code;
                        self.ubisoft_exe_dir = Some(prepared.exe_dir);
                        self.status =
                            "Token request captured. Send the activation code in your Ubisoft ticket.".into();
                        self.status_error = false;
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.ubisoft_prepare_receiver = None,
        }
    }

    /// Downloads the bot's response token and installs its `token.ini` payload next to the game exe.
    /// Reuses the shared verify receiver/poll (the same signed-token install path as Steam).
    fn verify_ubisoft_response(&mut self, app_id: u32) {
        if self.activation_verify_receiver.is_some() {
            return;
        }
        let Some(target) = self.ubisoft_exe_dir.clone() else {
            self.status = "Prepare the Ubisoft activation first.".into();
            self.status_error = true;
            return;
        };
        let response_code = self.response_code.concat();
        self.verified_entitlement = None;
        self.busy_label = Some("Verifying and installing token.ini…".into());
        let settings_directory = self.paths.settings_dir();
        let (sender, receiver) = mpsc::channel();
        self.activation_verify_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = ActivationRequestService::new(settings_directory)
                .and_then(|service| service.download_and_install(&response_code, app_id, &target))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn start_cloud_download(&mut self) {
        if self.cloud_download_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.cloud_download_receiver = Some(receiver);
        self.status = "Downloading CloudRedirect…".into();
        self.status_error = false;
        std::thread::spawn(move || {
            let result = CloudRedirect::new()
                .and_then(|client| client.download_dll())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    /// Poll the background CloudRedirect download; deploy the verified DLL beside `steam.exe`.
    fn poll_cloud_download(&mut self) {
        let Some(receiver) = &self.cloud_download_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.cloud_download_receiver = None;
                match result {
                    Ok(download) => match self.steam.root.clone() {
                        Some(root) => match cloud::deploy_dll(&root, &download.bytes) {
                            Ok(()) => {
                                self.status =
                                    format!("CloudRedirect {} installed next to steam.exe", download.version);
                                self.status_error = false;
                            }
                            Err(error) => {
                                self.status = format!("CloudRedirect could not be installed: {error}");
                                self.status_error = true;
                            }
                        },
                        None => {
                            self.status =
                                "Downloaded, but no Steam folder is set — configure it in Settings".into();
                            self.status_error = true;
                        }
                    },
                    Err(error) => {
                        self.status = format!("CloudRedirect download failed: {error}");
                        self.status_error = true;
                    }
                }
                self.refresh_cloud_state();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.cloud_download_receiver = None;
            }
        }
    }

    fn start_cloud_oauth(&mut self, provider: CloudProvider) {
        if self.cloud_oauth_receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.cloud_oauth_receiver = Some(receiver);
        self.status = format!("Opening {} sign-in in your browser…", provider.display_name());
        self.status_error = false;
        std::thread::spawn(move || {
            let result = cloud::authorize(provider, |_| {}).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    /// Poll the background OAuth sign-in; on success persist the provider config.
    fn poll_cloud_oauth(&mut self) {
        let Some(receiver) = &self.cloud_oauth_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.cloud_oauth_receiver = None;
                match result
                    .and_then(|settings| cloud::write_settings(&settings).map_err(|error| error.to_string()))
                {
                    Ok(()) => {
                        self.status = "Cloud sign-in complete and saved. Restart Steam to apply.".into();
                        self.status_error = false;
                    }
                    Err(error) => {
                        self.status = format!("Cloud sign-in failed: {error}");
                        self.status_error = true;
                    }
                }
                self.refresh_cloud_state();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.cloud_oauth_receiver = None;
            }
        }
    }

    fn cloud_page(&mut self, ui: &mut egui::Ui) {
        // Polled centrally in `update`, not here: a sign-in or DLL download started on this page and
        // left running while the user navigates away must still be collected, otherwise its receiver
        // stays occupied forever and blocks every later attempt.
        page_heading(ui, "CLOUD");
        ui.add_space(22.0);
        content_column(ui, CONTENT_WIDTH, |ui| {
            // What it is + the loud "back up your saves" warning.
            panel(ui, |ui| {
                section_label(ui, "STEAM CLOUD FOR LUA GAMES");
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "CloudRedirect gives lua games working Steam Cloud saves by redirecting \
                         cloud requests to a provider you choose.",
                    )
                    .size(12.0)
                    .color(TEXT),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "⚠ Experimental. Back up any saves you care about first — a bad sync can \
                         overwrite or lose them.",
                    )
                    .size(11.5)
                    .color(AMBER),
                );
            });

            // DLL install/update section.
            ui.add_space(16.0);
            panel(ui, |ui| {
                section_label(ui, "CLOUD REDIRECT");
                ui.add_space(8.0);
                let downloading = self.cloud_download_receiver.is_some();
                if downloading {
                    ui.ctx().request_repaint();
                }
                match self.steam.root.clone() {
                    Some(root) => {
                        // Cached snapshot (see `refresh_cloud_state`) — never hash the DLL per frame.
                        let status = self.cloud_dll_status.clone().unwrap_or_default();
                        if status.installed {
                            ui.label(RichText::new("Installed.").size(11.0).color(VERDIGRIS));
                        } else {
                            ui.label(RichText::new("Not installed.").size(11.0).color(MUTED));
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let button = if status.installed { "UPDATE" } else { "INSTALL" };
                            if ui
                                .add_enabled(!downloading, primary_button(button))
                                .on_hover_text(
                                    "Download the verified cloud_redirect.dll and place it beside steam.exe",
                                )
                                .clicked()
                            {
                                self.start_cloud_download();
                            }
                            if status.installed
                                && ui.add_enabled(!downloading, ghost_button("UNINSTALL")).clicked()
                            {
                                match cloud::remove_dll(&root) {
                                    Ok(()) => {
                                        self.status = "CloudRedirect DLL removed".into();
                                        self.status_error = false;
                                    }
                                    Err(error) => {
                                        self.status = format!("Could not remove the DLL: {error}");
                                        self.status_error = true;
                                    }
                                }
                                self.refresh_cloud_state();
                            }
                        });
                        if downloading {
                            ui.add_space(6.0);
                            ui.label(RichText::new("Downloading…").size(11.0).color(ACCENT_SOFT));
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new("Set your Steam folder in Settings first.")
                                .size(11.0)
                                .color(DANGER),
                        );
                    }
                }
            });

            // Provider configuration.
            ui.add_space(16.0);
            panel(ui, |ui| {
                section_label(ui, "CLOUD PROVIDER");
                ui.add_space(8.0);
                if let Some(current) = self.cloud_provider {
                    ui.label(
                        RichText::new(format!("Active: {}", current.display_name()))
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(6.0);
                }
                egui::ComboBox::from_id_salt("cloud_provider")
                    .selected_text(self.cloud.provider.display_name())
                    .width(280.0)
                    .show_ui(ui, |ui| {
                        for provider in [
                            CloudProvider::Folder,
                            CloudProvider::LocalOnly,
                            CloudProvider::GoogleDrive,
                            CloudProvider::OneDrive,
                            CloudProvider::S3,
                            CloudProvider::R2,
                        ] {
                            ui.selectable_value(&mut self.cloud.provider, provider, provider.display_name());
                        }
                    });
                ui.add_space(10.0);

                match self.cloud.provider {
                    CloudProvider::Folder => {
                        cloud_folder_row(
                            ui,
                            "Sync folder",
                            "A folder or mapped drive (e.g. a synced Google Drive / OneDrive folder)",
                            &mut self.cloud.folder_path,
                        );
                    }
                    CloudProvider::LocalOnly => {
                        ui.label(
                            RichText::new(
                                "Saves are staged locally only — no cloud sync, just clears the \
                                 Steam Cloud error. Leave the path empty for the default.",
                            )
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        cloud_folder_row(
                            ui,
                            "Local staging folder (optional)",
                            "Defaults to <Steam>/localcloud when empty",
                            &mut self.cloud.local_path,
                        );
                    }
                    CloudProvider::R2 => {
                        cloud_text_row(ui, "Account ID", &mut self.cloud.account_id, false);
                        cloud_text_row(ui, "Access Key ID", &mut self.cloud.access_key_id, false);
                        cloud_text_row(ui, "Secret Access Key", &mut self.cloud.secret_access_key, true);
                        cloud_text_row(ui, "Bucket", &mut self.cloud.bucket, false);
                        cloud_text_row(ui, "Key prefix (optional)", &mut self.cloud.key_prefix, false);
                    }
                    CloudProvider::S3 => {
                        cloud_text_row(ui, "Endpoint", &mut self.cloud.endpoint, false);
                        cloud_text_row(ui, "Region", &mut self.cloud.region, false);
                        cloud_text_row(ui, "Access Key ID", &mut self.cloud.access_key_id, false);
                        cloud_text_row(ui, "Secret Access Key", &mut self.cloud.secret_access_key, true);
                        cloud_text_row(ui, "Bucket", &mut self.cloud.bucket, false);
                        cloud_text_row(ui, "Key prefix (optional)", &mut self.cloud.key_prefix, false);
                    }
                    CloudProvider::GoogleDrive | CloudProvider::OneDrive => {
                        let provider = self.cloud.provider;
                        let in_progress = self.cloud_oauth_receiver.is_some();
                        if in_progress {
                            ui.ctx().request_repaint();
                        }
                        ui.label(
                            RichText::new(format!(
                                "{} uses a browser sign-in. Click below, complete the sign-in in \
                                 your browser, and the token is saved for the DLL automatically.",
                                provider.display_name()
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.add_space(10.0);
                        if ui
                            .add_enabled(!in_progress, primary_button("SIGN IN WITH BROWSER"))
                            .on_hover_text("Open the provider sign-in and save the token for the DLL")
                            .clicked()
                        {
                            self.start_cloud_oauth(provider);
                        }
                        if in_progress {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Waiting for the browser sign-in…")
                                    .size(11.0)
                                    .color(ACCENT_SOFT),
                            );
                        }
                    }
                }

                // The file-based providers save here; OAuth providers save on sign-in.
                if let Some(settings) = self.cloud.to_settings() {
                    ui.add_space(12.0);
                    if ui
                        .add(primary_button("SAVE CLOUD CONFIG"))
                        .on_hover_text("Write config.json (and credentials) so the DLL uses this provider")
                        .clicked()
                    {
                        match cloud::write_settings(&settings) {
                            Ok(()) => {
                                self.status = format!(
                                    "Cloud config saved for {}. Restart Steam to apply.",
                                    self.cloud.provider.display_name()
                                );
                                self.status_error = false;
                            }
                            Err(error) => {
                                self.status = format!("Could not save the cloud config: {error}");
                                self.status_error = true;
                            }
                        }
                        self.refresh_cloud_state();
                    }
                }
            });
        });
    }

    fn updates_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "UPDATES");
        ui.add_space(22.0);
        content_column(ui, CONTENT_WIDTH, |ui| {
            let previous_auto_update = self.settings.auto_update_drydock;
            let mut auto_update_changed = false;
            panel(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        section_label(ui, "DRYDOCK AUTO UPDATE");
                        ui.add_space(4.0);
                        let channel = if AppUpdater::configured_repository().is_some() {
                            "Verified downloads on launch"
                        } else {
                            "Release channel set in the official build"
                        };
                        ui.label(RichText::new(channel).size(9.5).color(ACCENT));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if toggle_switch(ui, &mut self.settings.auto_update_drydock, ACCENT_SOFT).changed() {
                            auto_update_changed = true;
                        }
                        ui.add_space(10.0);
                        if ui
                            .add_enabled(
                                AppUpdater::can_self_update() && self.update_receiver.is_none(),
                                ghost_button("CHECK NOW"),
                            )
                            .on_hover_text("Check the release channel for a verified Drydock update")
                            .clicked()
                        {
                            self.start_update_check(false);
                        }
                    });
                });
            });
            if auto_update_changed {
                match self.persist_settings() {
                    Ok(()) => {
                        self.status = if self.settings.auto_update_drydock {
                            "Drydock auto update enabled".into()
                        } else {
                            "Drydock auto update disabled".into()
                        };
                        self.status_error = false;
                    }
                    Err(error) => {
                        self.settings.auto_update_drydock = previous_auto_update;
                        self.status = format!("Auto-update setting could not be saved: {error}");
                        self.status_error = true;
                    }
                }
            }
            ui.add_space(16.0);
            section_label(ui, "STEAM GAME UPDATES");
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠  A Steam update can overwrite an applied fix or activation.")
                    .size(10.5)
                    .color(AMBER),
            );
            ui.add_space(12.0);

            let mut pending_change = None;
            for manifest in &self.manifests {
                let current = self
                    .settings
                    .steam_updates_enabled
                    .get(&manifest.app_id)
                    .copied()
                    .or_else(|| updates_enabled(&manifest.manifest_path).ok())
                    .unwrap_or(true);
                let mut preference = current;
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(12)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&manifest.name).size(15.0).strong().color(TEXT));
                                ui.label(
                                    RichText::new(format!("APP {}", manifest.app_id))
                                        .size(9.5)
                                        .color(ACCENT),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(manifest.manifest_path.display().to_string())
                                            .size(9.0)
                                            .color(MUTED),
                                    )
                                    .truncate(),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                // Track ON (cyan) = updates allowed; OFF (danger) = blocked/protected.
                                let accent = if preference { ACCENT_SOFT } else { DANGER };
                                if toggle_switch(ui, &mut preference, accent).changed() {
                                    pending_change = Some((
                                        manifest.app_id,
                                        manifest.manifest_path.clone(),
                                        current,
                                        preference,
                                    ));
                                }
                                ui.add_space(10.0);
                                if preference {
                                    status_pill(ui, "UPDATES ALLOWED", ACCENT_SOFT);
                                } else {
                                    status_pill(ui, "UPDATES BLOCKED", DANGER);
                                }
                            });
                        });
                    });
                ui.add_space(9.0);
            }
            if let Some((app_id, path, previous, enabled)) = pending_change {
                match set_manifest_updates_enabled(&path, enabled) {
                    Ok(()) => {
                        self.settings.steam_updates_enabled.insert(app_id, enabled);
                        match self.persist_settings() {
                            Ok(()) => {
                                self.status = if enabled {
                                    format!("Updates enabled for App {app_id}")
                                } else {
                                    format!("Updates blocked for App {app_id}")
                                };
                                self.status_error = false;
                            }
                            Err(error) => {
                                let rollback = set_manifest_updates_enabled(&path, previous);
                                self.settings.steam_updates_enabled.insert(app_id, previous);
                                self.status = match rollback {
                                    Ok(()) => {
                                        format!(
                                            "Update preference was not saved and was rolled back: {error}"
                                        )
                                    }
                                    Err(rollback_error) => format!(
                                        "Update preference was not saved and rollback failed: {error}; {rollback_error}"
                                    ),
                                };
                                self.status_error = true;
                            }
                        }
                    }
                    Err(error) => {
                        self.status = error.to_string();
                        self.status_error = true;
                    }
                }
            }
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "SETTINGS");
        ui.add_space(22.0);
        content_column(ui, CONTENT_WIDTH, |ui| {
            panel(ui, |ui| {
                section_label(ui, "STEAM FOLDER");
                ui.add_space(10.0);
                ui.add_sized(
                    [ui.available_width(), 42.0],
                    egui::TextEdit::singleline(&mut self.steam_directory_draft)
                        .hint_text("Steam directory")
                        .margin(egui::Margin::symmetric(12, 10)),
                );
                // Real-time validation indicator below the text field.
                match self.validate_steam_draft() {
                    SteamDirValidation::Empty => {}
                    SteamDirValidation::NotFound => {
                        ui.label(RichText::new("Folder does not exist").size(11.0).color(DANGER));
                    }
                    SteamDirValidation::MissingExecutable => {
                        ui.label(
                            RichText::new("Not a Steam folder — missing steam.exe / steam.sh")
                                .size(11.0)
                                .color(DANGER),
                        );
                    }
                    SteamDirValidation::MissingSteamapps => {
                        ui.label(
                            RichText::new("Steam executable found, but steamapps folder is missing")
                                .size(11.0)
                                .color(AMBER),
                        );
                    }
                    SteamDirValidation::Valid => {
                        ui.label(RichText::new("Steam folder found").size(11.0).color(VERDIGRIS));
                    }
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(primary_button("SAVE AND RELOAD"))
                        .on_hover_text("Save the Steam directory and reload installed games")
                        .clicked()
                        && self.save_settings()
                    {
                        self.refresh_steam();
                    }
                    if ui
                        .add(ghost_button("AUTO-DETECT"))
                        .on_hover_text("Automatically find the Steam installation folder")
                        .clicked()
                    {
                        let discovery = discover_steam(None);
                        if let Some(root) = discovery.root {
                            self.steam_directory_draft = root.display().to_string();
                            self.validated_steam_path = None;
                            self.status = "Steam folder detected".into();
                            self.status_error = false;
                        } else {
                            self.status = "Steam could not be detected automatically".into();
                            self.status_error = true;
                        }
                    }
                    if ui
                        .add(ghost_button("BROWSE"))
                        .on_hover_text("Open a file picker to select the Steam folder")
                        .clicked()
                    {
                        let mut dialog = rfd::FileDialog::new().set_title("Select Steam folder");
                        let current = std::path::Path::new(self.steam_directory_draft.trim());
                        if current.is_dir() {
                            dialog = dialog.set_directory(current);
                        }
                        if let Some(folder) = dialog.pick_folder() {
                            self.steam_directory_draft = folder.display().to_string();
                            self.validated_steam_path = None;
                            self.status = "Steam folder selected. Save to apply it.".into();
                            self.status_error = false;
                        }
                    }
                });
                ui.add_space(16.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(RichText::new("DATA FOLDER").size(9.5).strong().color(ACCENT));
                ui.label(
                    RichText::new(self.paths.settings_dir().display().to_string())
                        .size(10.0)
                        .color(MUTED),
                );
            });

            // The Steam Service controls (install / reinstall / uninstall / restart Steam) live here.
            ui.add_space(16.0);
            let service_action = steam_service_card(
                ui,
                &self.steam,
                self.service_status.as_ref(),
                self.service_receiver.is_some(),
                self.background_action.is_none(),
            );
            match service_action {
                SteamServiceCardAction::Install | SteamServiceCardAction::Reinstall => {
                    self.install_steam_service();
                }
                SteamServiceCardAction::Uninstall => self.uninstall_steam_service(),
                SteamServiceCardAction::Restart => self.restart_steam_in_background(),
                SteamServiceCardAction::None => {}
            }

            ui.add_space(16.0);
            panel(ui, |ui| {
                section_label(ui, "GAME LIST");
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{} games available", self.catalog.len())).color(TEXT));
                ui.add_space(10.0);
                let refreshing = self.catalog_receiver.is_some();
                if ui
                    .add_enabled(!refreshing, primary_button("FORCE REFRESH GAME LIST"))
                    .on_hover_text("Re-download the full game list (cached locally for 24 hours)")
                    .clicked()
                {
                    self.force_catalog_refresh();
                }
            });

            ui.add_space(16.0);
            panel(ui, |ui| {
                section_label(ui, "DOWNLOADS");
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Applies to new and resumed downloads (pause + resume to re-apply to a running one).",
                    )
                    .size(11.0)
                    .color(MUTED),
                );
                ui.add_space(12.0);

                // Max parallel CDN connections (0 in an old settings file migrates to the default 8).
                let mut connections = match self.settings.max_download_connections {
                    0 => 8,
                    n => n.clamp(1, 32),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Max connections").size(12.0).color(ACCENT));
                    ui.add(egui::Slider::new(&mut connections, 1..=32));
                });
                if connections != self.settings.max_download_connections {
                    self.settings.max_download_connections = connections;
                    self.status_error = self.persist_settings().is_err();
                }

                ui.add_space(10.0);
                let mut mbps = self.settings.max_download_mbps;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Max speed").size(12.0).color(ACCENT));
                    ui.add(
                        egui::DragValue::new(&mut mbps)
                            .range(0..=100_000)
                            .speed(1.0)
                            .suffix(" MB/s"),
                    );
                    ui.label(
                        RichText::new(if mbps == 0 { "unlimited" } else { "" })
                            .size(11.0)
                            .color(MUTED),
                    );
                });
                if mbps != self.settings.max_download_mbps {
                    self.settings.max_download_mbps = mbps;
                    self.status_error = self.persist_settings().is_err();
                }
            });

            ui.add_space(16.0);
            panel(ui, |ui| {
                section_label(ui, "CACHE");
                ui.add_space(6.0);
                ui.label(
                    RichText::new(self.paths.cache_dir().display().to_string())
                        .size(9.5)
                        .color(MUTED),
                );
                ui.add_space(10.0);
                if ui
                .add(ghost_button("CLEAR CACHE"))
                .on_hover_text(
                    "Delete cached game list, Denuvo list, and store artwork — safe; everything re-downloads. Settings and activation are kept.",
                )
                .clicked()
            {
                self.clear_drydock_cache();
            }
            });

            ui.add_space(16.0);
            self.self_hosting_panel(ui);

            // Only shown when the settings file on disk could not be parsed at startup. Until the
            // user decides, every save is blocked so their real library is not replaced by defaults.
            if self.settings_read_only {
                ui.add_space(16.0);
                panel(ui, |ui| {
                    section_label(ui, "BROKEN SETTINGS FILE");
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Your settings file could not be read, so Drydock started with defaults and \
                             is not saving anything. The original file was kept next to it — if your \
                             library is in there, close Drydock and repair it. Otherwise start fresh:",
                        )
                        .size(11.0)
                        .color(AMBER),
                    );
                    ui.add_space(10.0);
                    if ui
                        .add(ghost_button("DISCARD BROKEN SETTINGS"))
                        .on_hover_text(
                            "Start a new settings file from the current state. The unreadable one stays on disk.",
                        )
                        .clicked()
                    {
                        self.discard_broken_settings();
                    }
                });
            }
        });
    }

    /// Settings panel for pointing this build at a self-hosted proxy or a fork's release channel.
    ///
    /// Drydock's proxy is open source and meant to be run by anyone, so the address, the shared
    /// secret and the update repository must be changeable **without rebuilding**. Each field is
    /// blank by default, which means "use whatever this build was compiled with"; an environment
    /// variable of the same name still wins over anything entered here (see `drydock_core::config`).
    fn self_hosting_panel(&mut self, ui: &mut egui::Ui) {
        panel(ui, |ui| {
            section_label(ui, "PROXY / SELF-HOSTING");
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Leave these empty to use the values this build ships with. Fill them in to \
                     point Drydock at your own proxy — see proxy/README.md for running one.",
                )
                .size(11.0)
                .color(MUTED),
            );
            ui.add_space(10.0);

            let mut changed = false;
            let mut field = |ui: &mut egui::Ui, label: &str, hint: &str, value: &mut String, secret: bool| {
                ui.label(RichText::new(label).size(9.5).strong().color(ACCENT));
                ui.add_space(3.0);
                let edit = egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .password(secret)
                    .desired_width(f32::INFINITY);
                if ui.add(edit).changed() {
                    changed = true;
                }
                ui.add_space(10.0);
            };
            field(
                ui,
                "PROXY BASE URL",
                "https://proxy.example  (origin only, no path)",
                &mut self.settings.proxy_base_url,
                false,
            );
            field(
                ui,
                "PROXY HMAC SECRET",
                "must match one of the proxy's DRYDOCK_HMAC_SECRET values",
                &mut self.settings.proxy_hmac_secret,
                true,
            );
            field(
                ui,
                "UPDATE REPOSITORY",
                "owner/repo  (leave empty unless you run your own release channel)",
                &mut self.settings.update_repository,
                false,
            );

            if changed {
                // Apply immediately so the next request uses the new address — no restart needed.
                self.settings.apply_config_overrides();
                let _ = self.persist_settings();
            }

            // Show what actually resolved, so a typo or a stray environment variable is visible
            // rather than presenting as "the proxy is down".
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new("RESOLVED").size(9.5).strong().color(ACCENT));
            ui.add_space(4.0);
            for entry in drydock_core::describe_config() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}:", entry.name)).size(10.5).color(MUTED));
                    ui.label(RichText::new(&entry.value).size(10.5).color(TEXT));
                    ui.label(
                        RichText::new(format!("({})", entry.source.label()))
                            .size(10.0)
                            .color(if entry.source == ConfigSource::Unset {
                                DANGER
                            } else {
                                MUTED
                            }),
                    );
                });
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("Run `Drydock --config` for the same report on the command line.")
                    .size(10.0)
                    .color(MUTED),
            );
        });
    }

    /// Deletes the on-disk cache and drops the in-memory caches derived from it so they refill.
    fn clear_drydock_cache(&mut self) {
        match self.paths.clear_cache() {
            Ok(freed) => {
                self.denuvo_appids.clear();
                self.denuvo_loaded = false;
                self.start_denuvo_refresh(true);
                self.status = format!("Cache cleared — {} freed.", human_bytes(freed));
                self.status_error = false;
            }
            Err(error) => {
                self.status = format!("Cache could not be cleared: {error}");
                self.status_error = true;
            }
        }
    }

    fn guide_page(&mut self, ui: &mut egui::Ui) {
        if back_button(ui, "Return to the game library").clicked() {
            self.page = Page::Home;
            return;
        }
        ui.add_space(14.0);
        page_heading(ui, "HOW IT WORKS");
        ui.add_space(18.0);

        // Segmented switch between the two walkthroughs.
        egui::Frame::new()
            .fill(Color32::from_rgb(18, 21, 24))
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(11)
            .inner_margin(4)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if guide_tab(ui, "ACTIVATION", self.guide_flow == GuideFlow::Activation).clicked() {
                        self.guide_flow = GuideFlow::Activation;
                    }
                    if guide_tab(ui, "FIXES", self.guide_flow == GuideFlow::Fixes).clicked() {
                        self.guide_flow = GuideFlow::Fixes;
                    }
                });
            });
        ui.add_space(20.0);

        // Each flow is three clear stages so the whole process reads at a glance.
        type GuideStep = (&'static str, &'static str);
        type GuideStage = (&'static str, &'static str, &'static [GuideStep]);
        const ACTIVATION_STAGES: [GuideStage; 3] = [
            (
                "PREPARE",
                "Get your library and a game ready.",
                &[
                    (
                        "Set your Steam folder",
                        "Open Settings, point Drydock at your Steam folder, and it reads your installed games.",
                    ),
                    (
                        "Install the Steam Service",
                        "In Settings, install the Steam Service — it is required before a game can be added to Steam.",
                    ),
                    (
                        "Download the game fully",
                        "Add a supported game and let Steam finish downloading it completely before activating.",
                    ),
                ],
            ),
            (
                "ACTIVATE",
                "Turn a request code into a signed entitlement.",
                &[
                    (
                        "Generate a request code",
                        "Open Activation, pick the installed game, generate the request code, and copy it.",
                    ),
                    (
                        "Submit it in Discord",
                        "Open a Steam ticket in Discord, choose Activation Code, and paste the request code.",
                    ),
                    (
                        "Enter the response code",
                        "Type the eight-character response code the bot returns, then select Activate.",
                    ),
                ],
            ),
            (
                "PLAY",
                "Verified, protected, and ready.",
                &[
                    (
                        "Automatic verification",
                        "Drydock checks the signature, device, machine, App ID, lifetime, and payload integrity.",
                    ),
                    (
                        "Keep updates paused",
                        "Leave game updates disabled for the entitlement so it keeps working, then launch and play.",
                    ),
                ],
            ),
        ];
        const FIXES_STAGES: [GuideStage; 3] = [
            (
                "PREPARE",
                "Install the game you want to fix.",
                &[
                    (
                        "Set your Steam folder",
                        "Open Settings and point Drydock at your Steam folder so it can see your installed games.",
                    ),
                    (
                        "Install the Steam Service",
                        "In Settings, install the Steam Service — it must be current before a fix can be applied.",
                    ),
                    (
                        "Install the game fully",
                        "Add the game and let Steam finish downloading it completely, so there are files to patch.",
                    ),
                ],
            ),
            (
                "APPLY THE FIX",
                "One click installs everything the fix needs.",
                &[
                    (
                        "Open the Fixes tab",
                        "Pick a game that has a build-locked fix, then open its details page.",
                    ),
                    (
                        "Select Apply Fix",
                        "Drydock installs the matching unlock and downloads the fix files over your game install.",
                    ),
                    (
                        "Let it finish",
                        "Large fixes take a while to download and unpack — keep Drydock open until it reports success.",
                    ),
                ],
            ),
            (
                "PLAY",
                "Fixed, locked to a working build, and ready.",
                &[
                    (
                        "Updates are paused for you",
                        "The fix targets one game build, so Drydock blocks Steam updates to keep an update from breaking it.",
                    ),
                    (
                        "Launch and play",
                        "Start the game from Steam as usual — the fix is already in place.",
                    ),
                ],
            ),
        ];

        let stages: &[GuideStage] = match self.guide_flow {
            GuideFlow::Activation => &ACTIVATION_STAGES,
            GuideFlow::Fixes => &FIXES_STAGES,
        };
        let mut step = 1;
        for (index, (stage, stage_subtitle, steps)) in stages.iter().enumerate() {
            if index > 0 {
                ui.add_space(16.0);
            }
            guide_stage(ui, index + 1, stage, stage_subtitle, steps, &mut step);
        }
    }

    /// The Tools tab: a game-folder language changer. Pick the game (or repack) folder; Drydock scans
    /// its subfolders for the Steam-settings language files, lists the supported languages, and
    /// writes the chosen one. Greyed out with an explanation on non-Windows builds is not needed —
    /// language files are cross-platform — but the picker is always available here.
    fn tools_page(&mut self, ui: &mut egui::Ui) {
        page_heading(ui, "TOOLS");
        ui.add_space(22.0);
        content_column(ui, CONTENT_WIDTH, |ui| {
            panel(ui, |ui| {
                section_label(ui, "CHANGE GAME LANGUAGE");
                ui.add_space(10.0);
                let field_width = (ui.available_width() - 132.0).max(200.0);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [field_width, 40.0],
                        egui::TextEdit::singleline(&mut self.tools_language_path)
                            .hint_text("…\\steamapps\\common\\Game  (or the repack folder)")
                            .margin(egui::Margin::symmetric(12, 10)),
                    );
                    if ui
                        .add_sized([120.0, 40.0], ghost_button("CHOOSE FOLDER"))
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        self.tools_language_path = folder.display().to_string();
                        self.load_language_from(PathBuf::from(self.tools_language_path.trim()));
                    }
                });

                ui.add_space(12.0);
                if ui
                    .add_enabled(
                        !self.tools_language_path.trim().is_empty(),
                        ghost_button("SCAN FOLDER"),
                    )
                    .on_hover_text("Search this folder (and its subfolders) for language files")
                    .clicked()
                {
                    self.load_language_from(PathBuf::from(self.tools_language_path.trim()));
                }

                if let Some(options) = self.language_options.clone() {
                    ui.add_space(18.0);
                    section_label(ui, "LANGUAGE");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("tools_language_selection")
                            .selected_text(self.language_selection.to_uppercase())
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for language in &options.languages {
                                    ui.selectable_value(
                                        &mut self.language_selection,
                                        language.clone(),
                                        language.to_uppercase(),
                                    );
                                }
                            });
                        if ui
                            .add(primary_button("APPLY LANGUAGE"))
                            .on_hover_text("Write the selected language to this game")
                            .clicked()
                        {
                            self.apply_tools_language();
                        }
                    });
                }
            });

            ui.add_space(16.0);
            self.emu_template_panel(ui);
        });
    }

    /// The Tools emulator-cracker: builds a Cold Client Loader crack for a game entirely on the
    /// client (no Steam account). It resolves the `steam_settings` config (App ID, depots, DLCs,
    /// languages, achievements) and fetches the shared emu DLLs from the public GitHub releases
    /// (cached in AppData), then either deploys them into the game folder or saves a ZIP. The game
    /// exe is picked so the correct architecture (x64/x86) is used.
    fn emu_template_panel(&mut self, ui: &mut egui::Ui) {
        panel(ui, |ui| {
            section_label(ui, "STEAM EMU CRACKER (LOCAL)");
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Cracks a game with Cold Client Loader + gbe_fork — fully on your PC, no Steam \
                     account. Enter the App ID — Drydock writes the steam_settings and the \
                     matching-architecture emu DLLs, either straight into the game folder or as a \
                     ZIP. The architecture is auto-detected from Steam; the emu binaries are \
                     downloaded once and cached.",
                )
                .size(11.0)
                .color(MUTED),
            );
            ui.add_space(10.0);

            let busy = self.emu_receiver.is_some();
            ui.horizontal(|ui| {
                ui.label(RichText::new("App ID").size(11.0).color(ACCENT));
                ui.add_sized(
                    [140.0, 36.0],
                    egui::TextEdit::singleline(&mut self.emu_appid)
                        .hint_text("e.g. 2406770")
                        .margin(egui::Margin::symmetric(12, 8)),
                );
                ui.add_space(14.0);
                ui.label(RichText::new("Arch").size(11.0).color(ACCENT));
                ui.selectable_value(&mut self.emu_arch, EmuArch::Auto, "Auto");
                ui.selectable_value(&mut self.emu_arch, EmuArch::X64, "x64");
                ui.selectable_value(&mut self.emu_arch, EmuArch::X86, "x86");
                ui.add_space(14.0);
                ui.label(RichText::new("Loader").size(11.0).color(ACCENT));
                ui.selectable_value(&mut self.emu_loader_winmm, false, "version.dll");
                ui.selectable_value(&mut self.emu_loader_winmm, true, "winmm.dll");
            });

            ui.add_space(6.0);
            ui.checkbox(
                &mut self.emu_reframework,
                "Include REFramework (latest nightly dinput8.dll)",
            )
            .on_hover_text(
                "Adds praydog's REFramework next to the exe — only needed by some RE-Engine / \
                     Denuvo titles. Downloaded on demand and cached.",
            );

            ui.add_space(10.0);
            let valid = self.emu_appid.trim().parse::<u32>().is_ok_and(|id| id > 0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid && !busy, success_button("CRACK INTO GAME…"))
                    .on_hover_text("Pick the game's install folder — the crack is deployed into it")
                    .clicked()
                    && let Some(folder) = rfd::FileDialog::new()
                        .set_title("Pick the game's install folder")
                        .pick_folder()
                {
                    self.start_emu_crack(EmuOutput::Deploy(folder));
                }
                if ui
                    .add_enabled(valid && !busy, primary_button("SAVE AS ZIP…"))
                    .on_hover_text("Save the crack as a ZIP with the game's folder structure")
                    .clicked()
                    && let Some(zip) = rfd::FileDialog::new()
                        .add_filter("ZIP archive", &["zip"])
                        .set_file_name(format!("{}_crack.zip", self.emu_appid.trim()))
                        .save_file()
                {
                    self.start_emu_crack(EmuOutput::Zip(zip));
                }
                if busy {
                    ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                }
            });
            ui.add_space(8.0);
            if ui
                .add_enabled(!busy, ghost_button("RE-DOWNLOAD EMU FILES"))
                .on_hover_text("Force a fresh download of the Cold Client Loader / gbe_fork binaries")
                .clicked()
            {
                self.start_emu_toolchain_refresh();
            }
        });
    }

    /// Spawns the crack job: resolve config + architecture, ensure the emu toolchain, deploy or zip.
    fn start_emu_crack(&mut self, output: EmuOutput) {
        let Ok(app_id) = self.emu_appid.trim().parse::<u32>() else {
            return;
        };
        if self.emu_receiver.is_some() {
            return;
        }
        let skeleton = {
            let path = self.settings.emu_skeleton_path.trim();
            (!path.is_empty()).then(|| PathBuf::from(path))
        };
        let loader = if self.emu_loader_winmm {
            "winmm.dll"
        } else {
            "version.dll"
        };
        let arch = self.emu_arch;
        let reframework = self.emu_reframework;
        let cache_dir = self.paths.cache_dir();
        let (sender, receiver) = mpsc::channel();
        self.emu_receiver = Some(receiver);
        self.status = "Cracking — resolving config and emu files…".into();
        self.status_error = false;
        std::thread::spawn(move || {
            let _ = sender.send(build_emu_crack(
                app_id,
                &output,
                arch,
                loader,
                reframework,
                skeleton.as_deref(),
                &cache_dir,
            ));
        });
    }

    /// Force-refreshes the cached emu toolchain (Cold Client Loader / gbe_fork binaries).
    fn start_emu_toolchain_refresh(&mut self) {
        if self.emu_receiver.is_some() {
            return;
        }
        let cache_dir = self.paths.cache_dir();
        let (sender, receiver) = mpsc::channel();
        self.emu_receiver = Some(receiver);
        self.status = "Re-downloading the emu binaries…".into();
        self.status_error = false;
        std::thread::spawn(move || {
            let result = drydock_core::ensure_toolchain(&cache_dir, true)
                .map(|_| "Emu binaries re-downloaded.".to_owned())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn poll_emu_template(&mut self) {
        let Some(receiver) = self.emu_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.emu_receiver = None;
                match result {
                    Ok(message) => {
                        self.status = message;
                        self.status_error = false;
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.emu_receiver = None;
                self.status = "The template generator ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Writes the selected language to the folder loaded into the Tools language changer.
    fn apply_tools_language(&mut self) {
        let Some(directory) = self.language_directory.clone() else {
            return;
        };
        match apply_language(&directory, &self.language_selection) {
            Ok(language) => {
                self.status = format!("Language changed to {language}");
                self.status_error = false;
                self.load_language_from(directory);
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// The "crack / hypervisor files found" prompt shown when the pre-activation scan finds
    /// artifacts. Remove deletes them and continues to code generation; Cancel aborts.
    fn crack_removal_window(&mut self, context: &egui::Context) {
        let Some(pending) = &self.pending_crack else {
            return;
        };
        let count = pending.files.len();
        let names: Vec<String> = pending
            .files
            .iter()
            .take(14)
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect();
        let mut remove = false;
        let mut cancel = false;
        egui::Window::new("Crack / hypervisor files found")
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                ui.label(
                    RichText::new(
                        "Hypervisor- or crackfiles were found, the files need to be removed to activate it.",
                    )
                    .strong()
                    .color(DANGER),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{count} file(s)/folder(s) will be deleted from the game folder:"
                    ))
                    .size(10.5)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    for name in &names {
                        ui.label(RichText::new(format!("•  {name}")).size(10.5).color(TEXT));
                    }
                    if count > names.len() {
                        ui.label(
                            RichText::new(format!("…and {} more", count - names.len()))
                                .size(10.0)
                                .color(MUTED),
                        );
                    }
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(primary_button("REMOVE"))
                        .on_hover_text("Delete these files, then continue")
                        .clicked()
                    {
                        remove = true;
                    }
                    if ui.add(ghost_button("CANCEL")).clicked() {
                        cancel = true;
                    }
                });
            });
        if remove {
            self.start_crack_removal();
        } else if cancel {
            self.pending_crack = None;
            self.status = "Activation cancelled — crack files were not removed.".into();
            self.status_error = false;
        }
    }

    fn entitlement_success_window(&mut self, context: &egui::Context) {
        let Some(app_name) = self.entitlement_success_app.clone() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new(format!("{app_name} entitlement verified!"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(context, |ui| {
                ui.label(
                    RichText::new("The signed activation response is valid for this game and this device.")
                        .strong()
                        .color(ACCENT_SOFT),
                );
                ui.add_space(12.0);
                ui.label(RichText::new("We hope you have a great time with your game!").color(TEXT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Please take a moment to leave a review and share your experience with the community in Discord.",
                    )
                    .color(MUTED),
                );
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.add(primary_button("CLOSE")).on_hover_text("Close this dialog").clicked();
                });
            });
        if !open || close {
            self.entitlement_success_app = None;
        }
    }

    fn busy_overlay(&self, context: &egui::Context) {
        let Some(label) = &self.busy_label else {
            return;
        };
        let screen = context.content_rect();
        egui::Area::new("busy_overlay".into())
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(4, 5, 13, 220));
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(12.0);
                            ui.label(RichText::new(label).size(13.0).strong().color(TEXT));
                        });
                    });
                });
            });
    }
}

#[cfg(feature = "screenshot")]
impl DrydockApp {
    /// Ordered pages the screenshot harness walks through (see [`crate::screenshot`]).
    pub const SCREENSHOT_PAGES: [&'static str; 14] = [
        "home",
        "repacks",
        "denuvo",
        "library",
        "add-game",
        "tools",
        "cloud",
        "details",
        "activation",
        "updates",
        "settings",
        "guide",
        "guide-fixes",
        "downloads",
    ];

    /// Switches the visible page by key so the harness can capture each one.
    pub fn screenshot_goto(&mut self, key: &str) {
        match key {
            "details" => {
                // Prefer an installed game so the Play/Uninstall buttons are exercised;
                // otherwise fall back to the first catalog app.
                let app_id = self
                    .manifests
                    .first()
                    .map(|manifest| manifest.app_id)
                    .or_else(|| self.catalog.first().map(|app| app.app_id));
                if let Some(app_id) = app_id {
                    self.open_details(app_id);
                } else {
                    self.page = Page::Details;
                }
            }
            "denuvo" => {
                self.page = Page::Home;
                self.store_tab = StoreTab::DenuvoWatch;
            }
            "repacks" => {
                self.page = Page::Home;
                self.store_tab = StoreTab::Repacks;
            }
            "add-game" => {
                self.page = Page::Library;
                self.add_game_folder = Some(PathBuf::from("D:\\Games\\Onimusha Way of the Sword"));
            }
            "library" => self.page = Page::Library,
            "tools" => self.page = Page::Tools,
            "cloud" => self.page = Page::Cloud,
            "activation" => self.page = Page::Activation,
            "updates" => self.page = Page::Updates,
            "settings" => self.page = Page::Settings,
            "guide" => {
                self.guide_flow = GuideFlow::Activation;
                self.page = Page::Guide;
            }
            "guide-fixes" => {
                self.guide_flow = GuideFlow::Fixes;
                self.page = Page::Guide;
            }
            "downloads" => {
                // Seed a representative in-progress job so the page renders with real chrome. Keep
                // the sender alive (leak it) so the poll doesn't flip the job to a disconnected error
                // state — the screenshot then shows the true downloading layout.
                let (sender, receiver) = mpsc::channel::<DownloadUpdate>();
                std::mem::forget(sender);
                self.download_job = Some(DownloadJob {
                    app_id: 3_751_260,
                    name: "The Blood of Dawnwalker".into(),
                    kind: DownloadKind::Download,
                    cancel: Arc::new(AtomicBool::new(false)),
                    receiver,
                    progress: Some(DownloadProgress {
                        app_id: 3_751_260,
                        stage: drydock_core::DownloadStage::Downloading,
                        done_bytes: 1_288_490_188,
                        total_bytes: 10_737_418_240,
                        current_file: "Dawnwalker/Content/Paks/Dawnwalker-Windows.ucas".into(),
                    }),
                    speed_bps: 10_380_902.0,
                    peak_bps: 11_010_048.0,
                    sample: None,
                    finished: None,
                });
                self.page = Page::Downloads;
            }
            _ => self.page = Page::Home,
        }
    }
}

impl eframe::App for DrydockApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_background_action();
        self.poll_catalog_refresh();
        self.poll_denuvo_refresh();
        self.poll_fixes_refresh();
        self.poll_repacks_refresh();
        self.poll_store_details();
        self.poll_featured();
        self.poll_activation_request();
        self.poll_activation_check();
        self.poll_activation_remove();
        self.poll_ubisoft_prepare();
        self.poll_activation_verification();
        self.poll_service_action();
        self.poll_emu_template();
        self.poll_update();
        self.poll_download();
        self.poll_add_game();
        self.poll_download_install();
        self.poll_cloud_download();
        self.poll_cloud_oauth();
        // Entering a new page re-checks installed games and the Service status, so the
        // library, the activatable list, and the per-app buttons stay current without a restart.
        if self.page != self.last_page {
            self.last_page = self.page;
            self.refresh_dynamic_state();
            // Leaving a long list drops its header backlog — those rows are off screen now, and
            // working through them would keep hitting Steam for artwork nobody is looking at.
            self.header_resolver.cancel_pending();
        }
        let context = ui.ctx().clone();
        if self.exit_for_update {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Only repaint on user interaction or when background work is pending, plus a slow
        // refresh for the cosmetic backdrop animation.  Avoids burning CPU at 30 FPS when idle.
        let has_pending_background = self.catalog_receiver.is_some()
            || self.store_receiver.is_some()
            || self.featured_receiver.is_some()
            || self.activation_receiver.is_some()
            || self.activation_verify_receiver.is_some()
            || self.activation_check_receiver.is_some()
            || self.activation_remove_receiver.is_some()
            || self.ubisoft_prepare_receiver.is_some()
            || self.update_receiver.is_some()
            || self.service_receiver.is_some()
            || self.background_action.is_some()
            || self.denuvo_receiver.is_some()
            || self.fixes_receiver.is_some()
            || self.repacks_receiver.is_some()
            || self.emu_receiver.is_some()
            || self.add_game_receiver.is_some()
            || self.download_install_receiver.is_some()
            || self.cloud_download_receiver.is_some()
            || self.cloud_oauth_receiver.is_some()
            || self
                .download_job
                .as_ref()
                .is_some_and(|job| job.finished.is_none());
        // egui is reactive: it only repaints on input or when asked. vsync is off (see main.rs), so
        // the frame rate is capped here by scheduling the next repaint ourselves.
        //
        // Three tiers, because an app that mostly sits still should not cost a laptop its battery:
        //   * 60 FPS while the user is actually interacting (pointer moving, a click, a keystroke) or
        //     an animation is mid-flight, so hover and selection transitions stay smooth;
        //   * 30 FPS while focused but idle — enough for the ambient backdrop, half the GPU work;
        //   * 2 FPS when unfocused with nothing pending.
        // Background work keeps us at 30 FPS regardless of focus so progress bars still move.
        const INTERACTIVE_FRAME: std::time::Duration = std::time::Duration::from_micros(16_667); // 60 FPS
        const AMBIENT_FRAME: std::time::Duration = std::time::Duration::from_micros(33_333); // 30 FPS
        const IDLE_FRAME: std::time::Duration = std::time::Duration::from_millis(500); // 2 FPS
        let focused = context.input(|input| input.focused);
        let interacting = context.input(|input| {
            input.pointer.has_pointer() && (input.pointer.is_moving() || input.pointer.any_down())
                || !input.events.is_empty()
        });
        let animating = context.has_requested_repaint();
        let next_frame = if focused && (interacting || animating) {
            INTERACTIVE_FRAME
        } else if focused || has_pending_background {
            AMBIENT_FRAME
        } else {
            IDLE_FRAME
        };
        context.request_repaint_after(next_frame);
        self.top_nav(ui);
        self.status_bar(ui);
        egui::CentralPanel::default()
            // No side inner-margin: the scroll area spans the full width so its bar sits at the true
            // window edge. The horizontal gutter is applied inside, around a centred content column.
            .frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 18,
                bottom: 0,
            }))
            .show(ui, |ui| {
                paint_backdrop(ui, self.started.elapsed().as_secs_f32());
                egui::ScrollArea::vertical()
                    .id_salt("page_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Centre a capped-width column, with a minimum gutter on each side, so wide
                        // windows don't stretch the content and objects stay in one aligned block.
                        let avail = ui.available_width();
                        let capped = avail.min(CONTENT_WIDTH);
                        let side = ((avail - capped) / 2.0).max(MIN_CONTENT_GUTTER);
                        let content_w = (avail - side * 2.0).max(320.0);
                        ui.horizontal_top(|ui| {
                            ui.add_space(side);
                            ui.allocate_ui_with_layout(
                                Vec2::new(content_w, 0.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width(content_w);
                                    match self.page {
                                        Page::Home => self.home_page(ui),
                                        Page::Library => self.library_page(ui),
                                        Page::Details => self.details_page(ui),
                                        Page::Activation => self.activation_page(ui),
                                        Page::Tools => self.tools_page(ui),
                                        Page::Cloud => self.cloud_page(ui),
                                        Page::Updates => self.updates_page(ui),
                                        Page::Settings => self.settings_page(ui),
                                        Page::Guide => self.guide_page(ui),
                                        Page::Downloads => self.downloads_page(ui),
                                    }
                                },
                            );
                        });
                    });
            });
        self.crack_removal_window(&context);
        self.entitlement_success_window(&context);
        self.busy_overlay(&context);
    }
}

/// Registers an embedded CJK font as a fallback so non-Latin game titles (Chinese, Japanese,
/// Korean) render real glyphs instead of tofu boxes. Latin text keeps egui's default font; the
/// fallback is only consulted for code points the primary font has no glyph for.
fn install_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/NotoSansCJKsc-Regular.otf"
        ))),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("noto-cjk".to_owned());
    }
    context.set_fonts(fonts);
}

fn install_style(context: &egui::Context) {
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(16.0, 10.0);
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = SURFACE;
    style.visuals.window_stroke = Stroke::new(1.0, BORDER);
    style.visuals.window_corner_radius = egui::CornerRadius::same(16);
    style.visuals.extreme_bg_color = Color32::from_rgb(24, 28, 32);
    style.visuals.faint_bg_color = SURFACE_RAISED;
    style.visuals.widgets.inactive.bg_fill = SURFACE_RAISED;
    style.visuals.widgets.inactive.weak_bg_fill = SURFACE_RAISED;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.hovered.bg_fill = SURFACE_RAISED;
    style.visuals.widgets.hovered.weak_bg_fill = SURFACE_RAISED;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = ACCENT_DEEP;
    style.visuals.widgets.active.weak_bg_fill = ACCENT_DEEP;
    style.visuals.selection.bg_fill = lerp_color(BACKGROUND, ACCENT_DEEP, 0.55);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.override_text_color = Some(TEXT);
    // The scroll handle reads these foreground colours (see the floating scroll setup below),
    // giving a violet bar at rest that brightens on hover to match the app.
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_DEEP);
    style.visuals.text_cursor.stroke = Stroke::new(2.0, ACCENT);

    // Soften every widget's corners for a consistent, modern look.
    let radius = egui::CornerRadius::same(9);
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
    }

    // Thin, unobtrusive floating scrollbars, themed with the app's violet handle.
    let mut scroll = egui::style::ScrollStyle::floating();
    scroll.bar_width = 9.0;
    scroll.floating_width = 9.0;
    scroll.floating_allocated_width = 0.0;
    scroll.handle_min_length = 32.0;
    scroll.bar_inner_margin = 2.0;
    scroll.foreground_color = true;
    scroll.dormant_handle_opacity = 0.55;
    scroll.interact_handle_opacity = 1.0;
    scroll.active_handle_opacity = 1.0;
    scroll.dormant_background_opacity = 0.0;
    scroll.interact_background_opacity = 0.0;
    scroll.active_background_opacity = 0.0;
    style.spacing.scroll = scroll;

    context.set_style_of(egui::Theme::Dark, style);
    // Always render in our dark theme, regardless of the OS setting. Without this, a machine
    // set to a light system theme makes egui fall back to its light visuals for text fields,
    // combo boxes, scrollbars, and panel separators — which then show up white against the
    // app's dark chrome. Pinning both the preference and the fallback keeps the UI consistent.
    context.options_mut(|options| {
        options.theme_preference = egui::ThemePreference::Dark;
        options.fallback_theme = egui::Theme::Dark;
    });
}

fn paint_backdrop(ui: &mut egui::Ui, seconds: f32) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let drift = (seconds * 0.22).sin() * 26.0;
    // Two very faint washes drawn from the palette itself — brass above, verdigris below. The
    // previous pair was a hardcoded violet and cyan left over from the Steam-blue scheme; against a
    // brass accent the violet read as a different product's colour bleeding through.
    painter.circle_filled(
        egui::pos2(rect.right() - 110.0 + drift, rect.top() + 90.0),
        190.0,
        Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), 12),
    );
    painter.circle_filled(
        egui::pos2(rect.left() + 240.0 - drift, rect.bottom() - 60.0),
        150.0,
        Color32::from_rgba_unmultiplied(VERDIGRIS.r(), VERDIGRIS.g(), VERDIGRIS.b(), 10),
    );
}

/// A top-nav link tab: uppercase label, brightening on hover, with a brass underline when it is
/// the active page. Sized to its text so the nav packs tightly without wasting the bar.
fn nav_link(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), TEXT);
    let padding = Vec2::new(11.0, 0.0);
    let size = Vec2::new(galley.size().x + padding.x * 2.0, TOP_NAV_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let color = if active {
        Color32::WHITE
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    let text_pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(text_pos, galley, color);
    // The active underline (and a faint hover hint) sit on the bar's bottom edge.
    let underline = if active {
        ACCENT
    } else {
        lerp_color(Color32::TRANSPARENT, BORDER, hover)
    };
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 6.0, rect.bottom() - 2.0),
            egui::pos2(rect.right() - 6.0, rect.bottom() - 2.0),
        ],
        Stroke::new(2.0, underline),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A left-arrow back button used to return from a detail page to the list it was opened from.
fn back_button(ui: &mut egui::Ui, tooltip: &str) -> egui::Response {
    let size = Vec2::new(34.0, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = lerp_color(SURFACE, SURFACE_RAISED, hover);
    let stroke = Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover));
    ui.painter().rect(rect, 9, fill, stroke, egui::StrokeKind::Inside);
    let center = rect.center();
    let color = lerp_color(TEXT, Color32::WHITE, hover);
    let dx = 4.0;
    ui.painter().add(egui::Shape::line(
        vec![
            egui::pos2(center.x + dx, center.y - 8.0),
            egui::pos2(center.x - dx, center.y),
            egui::pos2(center.x + dx, center.y + 8.0),
        ],
        Stroke::new(2.2, color),
    ));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(tooltip)
}

/// A screenshot-carousel navigation button that paints a chevron and hovers.
/// A polished screenshot gallery: one large main image with subtle translucent circular arrows and
/// an index badge overlaid on it, plus a clickable thumbnail filmstrip below. Returns the
/// (possibly changed) index. The caller guarantees `screenshots` is non-empty.
fn screenshot_gallery(ui: &mut egui::Ui, screenshots: &[String], index: usize) -> usize {
    let count = screenshots.len();
    let mut new_index = index.min(count - 1);
    let corner = egui::CornerRadius::same(16);

    // Main image spans the full column width at a native 16:9 frame, filled edge-to-edge (cover) so
    // no letterbox bars ever appear. Overlay controls sit on top of it.
    let full_width = ui.available_width();
    let image_h = full_width * 0.5625;
    let (image_rect, _) = ui.allocate_exact_size(Vec2::new(full_width, image_h), Sense::hover());
    paint_remote_image_cover(ui, image_rect, &screenshots[new_index], corner);

    if count > 1 {
        let radius = 20.0;
        let cy = image_rect.center().y;
        if overlay_arrow(
            ui,
            egui::pos2(image_rect.left() + radius + 16.0, cy),
            radius,
            false,
        ) {
            new_index = if new_index == 0 { count - 1 } else { new_index - 1 };
        }
        if overlay_arrow(
            ui,
            egui::pos2(image_rect.right() - radius - 16.0, cy),
            radius,
            true,
        ) {
            new_index = (new_index + 1) % count;
        }

        // A rounded "current / total" badge tucked into the image's bottom-right corner.
        let galley = ui.painter().layout_no_wrap(
            format!("{} / {count}", new_index + 1),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        let badge_size = galley.size() + Vec2::new(20.0, 10.0);
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(
                image_rect.right() - 12.0 - badge_size.x,
                image_rect.bottom() - 12.0 - badge_size.y,
            ),
            badge_size,
        );
        ui.painter().rect_filled(
            badge_rect,
            egui::CornerRadius::same(255),
            Color32::from_black_alpha(170),
        );
        ui.painter()
            .galley(badge_rect.center() - galley.size() * 0.5, galley, Color32::WHITE);

        // Thumbnail filmstrip: scroll horizontally, click to jump, active one ringed in violet.
        ui.add_space(10.0);
        let thumb_h = 58.0;
        let thumb_w = thumb_h / 0.5625;
        let thumb_corner = egui::CornerRadius::same(8);
        egui::ScrollArea::horizontal()
            .id_salt("screenshot_strip")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (i, url) in screenshots.iter().enumerate() {
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(thumb_w, thumb_h), Sense::click());
                        paint_remote_image_cover(ui, rect, url, thumb_corner);
                        if i == new_index {
                            ui.painter().rect_stroke(
                                rect,
                                thumb_corner,
                                Stroke::new(2.0, ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        } else {
                            // Dim the ones you're not viewing; lift the dimming on hover.
                            let dim = if response.hovered() { 25 } else { 105 };
                            ui.painter()
                                .rect_filled(rect, thumb_corner, Color32::from_black_alpha(dim));
                            if response.hovered() {
                                ui.painter().rect_stroke(
                                    rect,
                                    thumb_corner,
                                    Stroke::new(1.5, lerp_color(BORDER, ACCENT, 0.6)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            new_index = i;
                        }
                    }
                });
            });
    }

    new_index
}

/// A small translucent circular navigation button overlaid on a screenshot edge. Returns whether
/// it was clicked.
fn overlay_arrow(ui: &mut egui::Ui, center: egui::Pos2, radius: f32, forward: bool) -> bool {
    let rect = egui::Rect::from_center_size(center, Vec2::splat(radius * 2.0));
    let response = ui.interact(rect, ui.id().with(("ss_arrow", forward)), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    ui.painter().circle_filled(
        center,
        radius,
        Color32::from_black_alpha((150.0 + 80.0 * hover) as u8),
    );
    ui.painter().circle_stroke(
        center,
        radius,
        Stroke::new(1.0, lerp_color(Color32::from_white_alpha(45), ACCENT, hover)),
    );
    let dx = if forward { 3.5 } else { -3.5 };
    ui.painter().add(egui::Shape::line(
        vec![
            egui::pos2(center.x - dx, center.y - 7.0),
            egui::pos2(center.x + dx, center.y),
            egui::pos2(center.x - dx, center.y + 7.0),
        ],
        Stroke::new(2.2, Color32::WHITE),
    ));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// Visual weight of a [`PillButton`].
#[derive(Clone, Copy)]
enum ButtonKind {
    Primary,
    Ghost,
    Success,
}

/// A rounded button that animates its fill/stroke on hover. Implements [`egui::Widget`]
/// so every existing `add`, `add_enabled`, and `add_sized` call site keeps working.
struct PillButton {
    label: String,
    kind: ButtonKind,
    min_size: Vec2,
}

impl PillButton {
    fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }
}

fn primary_button(label: &str) -> PillButton {
    PillButton {
        label: label.to_owned(),
        kind: ButtonKind::Primary,
        min_size: Vec2::ZERO,
    }
}

/// A labelled single-line text field for the Cloud provider form. `secret` masks the input.
fn cloud_text_row(ui: &mut egui::Ui, label: &str, value: &mut String, secret: bool) {
    ui.label(RichText::new(label).size(10.5).color(ACCENT));
    ui.add_sized(
        [ui.available_width(), 38.0],
        egui::TextEdit::singleline(value)
            .password(secret)
            .margin(egui::Margin::symmetric(12, 9)),
    );
    ui.add_space(8.0);
}

/// A labelled folder field with a Browse button that opens a native folder picker.
fn cloud_folder_row(ui: &mut egui::Ui, label: &str, hint: &str, value: &mut String) {
    ui.label(RichText::new(label).size(10.5).color(ACCENT));
    ui.horizontal(|ui| {
        ui.add_sized(
            [ui.available_width() - 96.0, 38.0],
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .margin(egui::Margin::symmetric(12, 9)),
        );
        if ui.add(ghost_button("BROWSE")).clicked() {
            let mut dialog = rfd::FileDialog::new().set_title(label);
            let current = std::path::Path::new(value.trim());
            if current.is_dir() {
                dialog = dialog.set_directory(current);
            }
            if let Some(folder) = dialog.pick_folder() {
                *value = folder.display().to_string();
            }
        }
    });
    ui.add_space(8.0);
}

fn ghost_button(label: &str) -> PillButton {
    PillButton {
        label: label.to_owned(),
        kind: ButtonKind::Ghost,
        min_size: Vec2::ZERO,
    }
}

/// A Steam-green call-to-action button, used for the Library's Play control.
fn success_button(label: &str) -> PillButton {
    PillButton {
        label: label.to_owned(),
        kind: ButtonKind::Success,
        min_size: Vec2::ZERO,
    }
}

impl egui::Widget for PillButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let enabled = ui.is_enabled();
        let font = FontId::proportional(10.5);
        let galley = ui
            .painter()
            .layout_no_wrap(self.label.clone(), font, Color32::WHITE);
        let padding = Vec2::new(16.0, 9.0);
        let mut size = (galley.size() + 2.0 * padding).max(self.min_size);
        size.y = size.y.max(32.0);
        // Fill the allocated box when placed by `add_sized` (a justified layout).
        let layout = *ui.layout();
        if layout.main_justify || layout.cross_justify {
            size = size.max(ui.available_size_before_wrap());
        }
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());

        let hover = if enabled {
            ui.ctx().animate_bool(response.id, response.hovered())
        } else {
            0.0
        };
        let (fill, stroke, text_color) = match self.kind {
            ButtonKind::Primary => (
                lerp_color(ACCENT_DEEP, ACCENT_SOFT, hover),
                Stroke::NONE,
                Color32::WHITE,
            ),
            ButtonKind::Ghost => (
                lerp_color(SURFACE, SURFACE_RAISED, hover),
                Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
                lerp_color(TEXT, Color32::WHITE, hover),
            ),
            ButtonKind::Success => (
                lerp_color(VERDIGRIS, Color32::from_rgb(110, 199, 168), hover),
                Stroke::NONE,
                Color32::from_rgb(16, 26, 22),
            ),
        };
        let (fill, stroke, text_color) = if enabled {
            (fill, stroke, text_color)
        } else {
            (
                lerp_color(fill, BACKGROUND, 0.45),
                Stroke::new(stroke.width, lerp_color(stroke.color, BACKGROUND, 0.5)),
                MUTED,
            )
        };

        if ui.is_rect_visible(rect) {
            ui.painter().rect(rect, 9, fill, stroke, egui::StrokeKind::Inside);
            let galley = ui
                .painter()
                .layout_no_wrap(self.label, FontId::proportional(10.5), text_color);
            let pos = rect.center() - galley.size() / 2.0;
            ui.painter().galley(pos, galley, text_color);
        }
        if enabled && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }
}

fn page_heading(ui: &mut egui::Ui, title: &str) {
    ui.add(egui::Label::new(RichText::new(title).size(30.0).strong().color(TEXT)).wrap());
}

/// Lays out page content in a left-aligned column capped at `max_width`, so forms stay a readable
/// width on wide/fullscreen windows while staying anchored to the left (the page scrollbar keeps to
/// the window edge, and cards don't float in the middle).
fn content_column(ui: &mut egui::Ui, max_width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width().min(max_width);
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        add_contents(ui);
    });
}

/// A dynamic game search: a heading-sized search field over a fixed-height results list. Returns the
/// App ID the user clicked, if any. The results box is a constant height (never resizing with the
/// match count), so neither it nor its scrollbar jumps as the query changes. When the query already
/// equals the selected game's name the list stays collapsed (the caller has its pick).
#[allow(clippy::too_many_arguments)]
fn game_search_box(
    ui: &mut egui::Ui,
    id: &str,
    search: &mut String,
    selected: Option<u32>,
    catalog: &[CatalogApp],
    headers: &HeaderResolver,
    width: f32,
    // When true (a filter is active) the results stay open even with an empty query, so the user can
    // browse everything the filter matches. `extra_filter` narrows the catalog before the text match.
    allow_empty: bool,
    extra_filter: impl Fn(&CatalogApp) -> bool,
) -> Option<u32> {
    let mut clicked = None;
    // Natural height with a slightly heavier top margin so the text sits optically centred (egui
    // top-aligns the galley within the line box, which otherwise reads a touch high).
    ui.add(
        egui::TextEdit::singleline(search)
            .hint_text("Search by name or App ID")
            .font(egui::TextStyle::Heading)
            .margin(egui::Margin {
                left: 18,
                right: 18,
                top: 16,
                bottom: 12,
            })
            .desired_width(width),
    );

    let query = search.trim().to_lowercase();
    let selected_name = selected
        .and_then(|app_id| catalog.iter().find(|entry| entry.app_id == app_id))
        .map(|entry| entry.name.to_lowercase());
    // Without an active filter, an empty query (or re-selecting the current pick) collapses the list.
    if !allow_empty && (query.is_empty() || selected_name.as_deref() == Some(query.as_str())) {
        return None;
    }

    let matches: Vec<&CatalogApp> = catalog
        .iter()
        .filter(|entry| {
            extra_filter(entry)
                && (query.is_empty()
                    || entry.name.to_lowercase().contains(&query)
                    || entry.app_id.to_string().contains(&query))
        })
        .take(100)
        .collect();

    ui.add_space(8.0);
    egui::Frame::new()
        .fill(SURFACE_RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(8)
        .show(ui, |ui| {
            // Subtract the frame's inner margin (8px each side) so the panel's OUTER width matches
            // the search field above it exactly, instead of overhanging by the margins.
            ui.set_width((width - 16.0).max(1.0));
            ui.set_height(360.0);
            if matches.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new("No matching games").size(12.0).color(MUTED));
                    });
            } else {
                // Fixed-height rows via `show_rows` so only the visible results are laid out — this
                // keeps the header-image thumbnails to the handful on screen instead of fetching one
                // per match.
                let row_height = 50.0;
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, matches.len(), |ui, range| {
                        for index in range {
                            let entry = matches[index];
                            if search_result_row(
                                ui,
                                entry,
                                row_height,
                                selected == Some(entry.app_id),
                                headers,
                            ) {
                                clicked = Some(entry.app_id);
                            }
                        }
                    });
            }
        });
    clicked
}

/// Whether a catalog app passes the active Home filters. Both filters must pass (AND). The lookups
/// are the ones [`DrydockApp::rebuild_repack_index`] / [`DrydockApp::rebuild_fix_index`] maintain.
fn catalog_matches_filters(
    entry: &CatalogApp,
    repack_filter: &RepackFilter,
    fix_filter: FixFilter,
    repackers_by_app: &std::collections::HashMap<u32, Vec<String>>,
    fix_flags_by_app: &std::collections::HashSet<u32>,
) -> bool {
    let repack_ok = match repack_filter {
        RepackFilter::Any => true,
        RepackFilter::AnyRepack => repackers_by_app.contains_key(&entry.app_id),
        RepackFilter::Repacker(name) => {
            let name = name.to_lowercase();
            repackers_by_app
                .get(&entry.app_id)
                .is_some_and(|list| list.iter().any(|entry| entry == &name))
        }
    };
    if !repack_ok {
        return false;
    }
    match fix_filter {
        FixFilter::Any => true,
        FixFilter::Denuvo => fix_flags_by_app.contains(&entry.app_id),
    }
}

/// One search-result row: a small Steam header thumbnail on the left, then the game name and App
/// ID stacked. Returns true when clicked. Called only for on-screen rows (via `show_rows`), so a
/// thumbnail is only ever fetched for a visible result.
/// One app's resolved header state in the [`HeaderResolver`] map.
enum HeaderSlot {
    /// A resolution is in flight (or queued) — don't enqueue it again.
    Pending,
    /// The real `header_image` URL from Steam's `appdetails`.
    Resolved(String),
    /// `appdetails` had no usable header (unlikely, e.g. a delisted app).
    Failed,
}

/// How many resolver threads run in parallel. Each pulls App IDs off the shared queue and calls
/// `appdetails`; the store's token-bucket limiter still caps the aggregate request rate, so this only
/// governs how many can be in flight at once (a small burst resolves together instead of one-by-one).
const HEADER_RESOLVER_WORKERS: usize = 4;

/// Upper bound on queued-but-unstarted header resolutions.
///
/// Scrolling a long list fast used to enqueue every row whose CDN guess failed — hundreds of App IDs,
/// which at the store's sustained refill rate meant ten-plus minutes of `appdetails` traffic that kept
/// running long after the user had left the page. The queue is now a bounded *most-recently-requested*
/// window: a push past the cap drops the oldest entry, because the oldest request is the one least
/// likely to still be on screen.
const HEADER_RESOLVER_QUEUE_LIMIT: usize = 48;

/// Resolves a catalog row's real header art on demand.
///
/// The App-ID-derived CDN URLs ([`steam_artwork_urls`]) 404 for titles Steam migrated to hashed
/// `store_item_assets` paths, so those rows would otherwise show a blank tile. This asks Steam's
/// `appdetails` for the current `header_image` — one lightweight request per app, cached on disk for
/// two weeks (see [`SteamStoreClient::header_image`]). Resolution is only ever requested for rows
/// whose cheap CDN guesses failed, runs on a small pool of background workers so several resolve in
/// parallel, and never blocks the render thread, which only reads the cached result.
#[derive(Clone)]
struct HeaderResolver {
    slots: Arc<Mutex<HashMap<u32, HeaderSlot>>>,
    queue: Arc<(Mutex<std::collections::VecDeque<u32>>, std::sync::Condvar)>,
}

impl HeaderResolver {
    fn new(cache_directory: PathBuf, ctx: egui::Context) -> Self {
        let slots: Arc<Mutex<HashMap<u32, HeaderSlot>>> = Arc::new(Mutex::new(HashMap::new()));
        let queue: Arc<(Mutex<std::collections::VecDeque<u32>>, std::sync::Condvar)> = Arc::new((
            Mutex::new(std::collections::VecDeque::new()),
            std::sync::Condvar::new(),
        ));
        for _ in 0..HEADER_RESOLVER_WORKERS {
            let worker_slots = Arc::clone(&slots);
            let worker_queue = Arc::clone(&queue);
            let cache_directory = cache_directory.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let Ok(client) = SteamStoreClient::new(&cache_directory) else {
                    return;
                };
                loop {
                    // Take the next queued App ID, waiting on the condvar while the queue is empty.
                    let app_id = {
                        let (lock, cvar) = &*worker_queue;
                        let mut queue = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        while queue.is_empty() {
                            queue = cvar
                                .wait(queue)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                        queue.pop_front().expect("queue non-empty after wait")
                    };
                    // The network call happens outside every lock, so the workers actually overlap
                    // (bounded only by the store's shared request limiter).
                    let slot = match client.header_image(app_id) {
                        Ok(url) => HeaderSlot::Resolved(url),
                        Err(_) => HeaderSlot::Failed,
                    };
                    if let Ok(mut map) = worker_slots.lock() {
                        map.insert(app_id, slot);
                    }
                    // Wake the UI so freshly-resolved art paints even when the window is unfocused.
                    ctx.request_repaint();
                }
            });
        }
        Self { slots, queue }
    }

    /// The resolved header URL if one is already known — never triggers a request. Returns `None`
    /// while unknown, pending, or failed.
    fn get(&self, app_id: u32) -> Option<String> {
        let map = self.slots.lock().ok()?;
        match map.get(&app_id) {
            Some(HeaderSlot::Resolved(url)) => Some(url.clone()),
            _ => None,
        }
    }

    /// Enqueues a one-off background resolution the first time an app is requested. A no-op once the
    /// app is pending, resolved, or known-failed, so it is safe to call every frame. Only rows whose
    /// cheap CDN guesses fail ever call this, keeping `appdetails` traffic minimal (and well under
    /// Steam's rate limit).
    ///
    /// The queue is capped at [`HEADER_RESOLVER_QUEUE_LIMIT`]; a request that overflows it evicts the
    /// oldest pending App ID (and its `Pending` marker, so it can be asked for again if still
    /// visible). Scrolling therefore keeps the work focused on what the user is actually looking at
    /// instead of working through a long backlog of rows that have scrolled away.
    fn request(&self, app_id: u32) {
        let Ok(mut map) = self.slots.lock() else {
            return;
        };
        if map.contains_key(&app_id) {
            return;
        }
        map.insert(app_id, HeaderSlot::Pending);
        drop(map);
        let (lock, cvar) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.push_back(app_id);
            while queue.len() > HEADER_RESOLVER_QUEUE_LIMIT {
                if let Some(dropped) = queue.pop_front()
                    && let Ok(mut map) = self.slots.lock()
                {
                    // Forget the marker too: the row may come back into view and should then be
                    // re-requested rather than staying `Pending` forever.
                    map.remove(&dropped);
                }
            }
            cvar.notify_one();
        }
    }

    /// Drops everything still waiting to be resolved, e.g. when the user leaves a long list.
    ///
    /// Work already in flight finishes (it is one cheap request), but the backlog does not outlive the
    /// page that created it.
    fn cancel_pending(&self) {
        let (lock, _) = &*self.queue;
        let Ok(mut queue) = lock.lock() else {
            return;
        };
        let dropped: Vec<u32> = queue.drain(..).collect();
        drop(queue);
        if let Ok(mut map) = self.slots.lock() {
            for app_id in dropped {
                map.remove(&app_id);
            }
        }
    }
}

fn search_result_row(
    ui: &mut egui::Ui,
    entry: &CatalogApp,
    row_height: f32,
    selected: bool,
    headers: &HeaderResolver,
) -> bool {
    let (slot, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_height), Sense::click());
    // The card fills the top of the slot; the bottom LIST_ROW_GAP is left empty so rows separate the
    // same way the Featured cards do. Keeping the allocated slot at `row_height` means callers and
    // `show_rows` still advance by exactly one row.
    // The card fills the whole slot; the gap between rows comes from the caller's `item_spacing.y`.
    let rect = slot;
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    // A persistent card (not just a hover tint) so the gap between rows actually reads — transparent
    // rows on a flat panel show no visible spacing however big the gap is.
    let fill = if selected {
        lerp_color(SURFACE_RAISED, ACCENT, 0.22)
    } else {
        lerp_color(SURFACE, SURFACE_RAISED, 0.35 + hover * 0.65)
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        Stroke::new(
            1.0,
            lerp_color(BORDER, ACCENT, if selected { 0.6 } else { hover }),
        ),
        egui::StrokeKind::Inside,
    );

    let pad = 8.0;
    let img_h = (rect.height() - pad * 2.0).max(1.0);
    let img_w = img_h / STEAM_HEADER_ASPECT;
    let img_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad, rect.top() + pad),
        Vec2::new(img_w, img_h),
    );
    // Only touch artwork for rows near the viewport; off-screen rows draw a neutral tile.
    if ui.clip_rect().expand(row_height * 8.0).intersects(slot) {
        // Cheap App-ID CDN guesses first (header → capsule → library) — most games render here with
        // no API call at all. The `appdetails`-resolved URL is appended only as a last resort and is
        // requested only when the guesses actually fail, so a browse/search never fires a burst of
        // `appdetails` calls that could trip Steam's per-IP rate limit.
        let guesses = steam_artwork_urls(entry.app_id);
        let resolved = headers.get(entry.app_id);
        let mut refs: Vec<&str> = guesses.iter().map(String::as_str).collect();
        if let Some(url) = resolved.as_deref() {
            refs.push(url);
        }
        // Cover-fit so a non-header aspect (a capsule fallback) fills the thumb with no bars.
        let all_failed = paint_remote_image_cover_multi(ui, img_rect, &refs, egui::CornerRadius::same(6));
        if all_failed {
            headers.request(entry.app_id);
        }
    } else {
        ui.painter()
            .rect_filled(img_rect, egui::CornerRadius::same(6), SURFACE);
    }

    // Clip text to the row so a very long name can never bleed into neighbours.
    let text_x = img_rect.right() + 12.0;
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        egui::pos2(text_x, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        &entry.name,
        FontId::proportional(14.0),
        TEXT,
    );
    painter.text(
        egui::pos2(text_x, rect.center().y + 9.0),
        egui::Align2::LEFT_CENTER,
        format!("APP {}", entry.app_id),
        FontId::proportional(10.0),
        MUTED,
    );

    response.clicked()
}

/// Whether an installed app is a real game worth surfacing in the Home library, rather than one of
/// Steam's own runtimes/redistributables that show up as installed "apps".
fn is_real_game(app_id: u32, name: &str) -> bool {
    // Steamworks Common Redistributables, Steam Linux Runtime(s), various Proton builds.
    const TOOL_APP_IDS: &[u32] = &[228_980, 1_070_560, 1_391_110, 1_628_350];
    if TOOL_APP_IDS.contains(&app_id) {
        return false;
    }
    let lower = name.to_lowercase();
    !(lower.contains("redistributable")
        || lower.contains("steam linux runtime")
        || lower.starts_with("proton"))
}

/// A card-styled filter dropdown: a small uppercase caption above a bordered combo box of fixed
/// `width`, so the Home filters match the surrounding cards instead of egui's default widget look.
fn filter_dropdown(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    selected: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(9.0).strong().color(MUTED));
        ui.add_space(5.0);
        // Restyle the combo (button + popup) to the app's card palette for this widget only.
        let mut style = (**ui.style()).clone();
        let round = egui::CornerRadius::same(10);
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = round;
            widget.bg_fill = SURFACE_RAISED;
            widget.weak_bg_fill = SURFACE_RAISED;
            widget.bg_stroke = Stroke::new(1.0, BORDER);
            widget.expansion = 0.0;
        }
        let lit = Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.55));
        style.visuals.widgets.hovered.bg_stroke = lit;
        style.visuals.widgets.hovered.weak_bg_fill = lerp_color(SURFACE_RAISED, ACCENT, 0.10);
        style.visuals.widgets.active.bg_stroke = lit;
        style.visuals.widgets.open.bg_stroke = lit;
        style.spacing.button_padding = Vec2::new(12.0, 8.0);
        style.spacing.combo_width = width;
        ui.set_style(style);
        egui::ComboBox::from_id_salt(id)
            .selected_text(RichText::new(selected).size(12.0).color(TEXT))
            .width(width)
            .show_ui(ui, add_contents);
    });
}

/// What a storefront capsule/hero click asks the Store page to do (applied after the feed borrow).
enum StoreAction {
    Details(u32),
    Activate(u32),
}

/// A Steam-style store sub-tab: a pill that fills in when active, sized to its label.
fn store_subtab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, TEXT);
    let size = Vec2::new(galley.size().x + 26.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        SURFACE_RAISED
    } else {
        lerp_color(Color32::TRANSPARENT, SURFACE, hover)
    };
    let stroke = if active {
        Stroke::new(1.0, BORDER)
    } else {
        Stroke::NONE
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius {
            nw: 7,
            ne: 7,
            sw: 0,
            se: 0,
        },
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let color = if active {
        Color32::WHITE
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The full-width Featured banner: a wide cinematic strip for the #1 seller with a left-anchored
/// title/price/CTA block over a left-darkening gradient, in the style of Steam's store carousel.
fn store_banner(
    ui: &mut egui::Ui,
    capsule: &StoreCapsule,
    rank: usize,
    activatable: bool,
) -> Option<StoreAction> {
    let mut action = None;
    let width = ui.available_width();
    // Height follows the hero image's aspect so `library_hero.jpg` fills the banner exactly — no
    // letterbox bars — and the banner is as wide as the (column-capped) image.
    let height = width / STORE_HERO_ASPECT;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let corner = egui::CornerRadius::same(12);
    // Use the widest capsule art Steam has for this title, falling back to the feed's header.
    let urls = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
            capsule.app_id
        ),
        capsule.header_image_url.clone(),
    ];
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    // Cover-fit (fill + crop) so any art fills the banner edge to edge instead of leaving bars.
    paint_remote_image_cover_multi(ui, rect, &refs, corner);
    let response = ui.interact(rect, ui.id().with(("banner", capsule.app_id)), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let painter = ui.painter().with_clip_rect(rect);
    // Left→right darkening *gradient* so the copy stays legible over any hero art. A soft fade to
    // transparent (not a hard-edged rectangle) avoids a visible seam on evenly-lit art.
    let fade_w = width * 0.70;
    let strips = 48;
    for i in 0..strips {
        let t = i as f32 / (strips - 1) as f32;
        let alpha = (170.0 * (1.0 - t).powf(1.3)) as u8;
        if alpha == 0 {
            continue;
        }
        let x0 = rect.left() + fade_w * (i as f32 / strips as f32);
        let x1 = rect.left() + fade_w * ((i + 1) as f32 / strips as f32);
        let round = if i == 0 {
            egui::CornerRadius {
                nw: 12,
                sw: 12,
                ne: 0,
                se: 0,
            }
        } else {
            egui::CornerRadius::ZERO
        };
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            round,
            Color32::from_rgba_unmultiplied(8, 12, 18, alpha),
        );
    }
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
        egui::StrokeKind::Inside,
    );
    let left = rect.left() + 30.0;
    // Rank + Activatable eyebrow line.
    let mut eyebrow = format!("#{rank} TOP SELLER");
    if activatable {
        eyebrow.push_str("   ·   ACTIVATABLE IN DRYDOCK");
    }
    painter.text(
        egui::pos2(left, rect.top() + 40.0),
        egui::Align2::LEFT_TOP,
        eyebrow,
        FontId::monospace(11.0),
        if activatable { ACCENT_SOFT } else { MUTED },
    );
    painter.text(
        egui::pos2(left, rect.top() + 60.0),
        egui::Align2::LEFT_TOP,
        &capsule.name,
        FontId::proportional(34.0),
        Color32::WHITE,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        action = Some(StoreAction::Details(capsule.app_id));
    }
    // The CTA button, anchored bottom-left.
    let (label, primary) = if activatable {
        ("ACTIVATE IN DRYDOCK", true)
    } else {
        ("VIEW IN STORE", false)
    };
    let btn_w = if activatable { 188.0 } else { 150.0 };
    let btn_rect = egui::Rect::from_min_size(egui::pos2(left, rect.bottom() - 52.0), Vec2::new(btn_w, 34.0));
    let button = if primary {
        success_button(label).min_size(Vec2::new(btn_w, 34.0))
    } else {
        ghost_button(label).min_size(Vec2::new(btn_w, 34.0))
    };
    if ui.put(btn_rect, button).clicked() {
        action = Some(if activatable {
            StoreAction::Activate(capsule.app_id)
        } else {
            StoreAction::Details(capsule.app_id)
        });
    }
    action
}

/// Height of a Steam-style list row (thumbnail + title + meta + a right-hand control).
const LIST_ROW_HEIGHT: f32 = 62.0;
/// Transparent gap baked below each row so a visible space always separates the cards, independent
/// of layout item-spacing (which the right-hand `ui.put` control otherwise swallows).
const LIST_ROW_GAP: f32 = 5.0;

/// Draws the shared chrome of a Steam-style list row — a full-width band with a small landscape
/// thumbnail, a title and a meta line — and returns the rect reserved on the right for a control
/// plus a click response for the rest of the row (used to open details). `right_w` is the width to
/// reserve on the right; pass 0.0 for a row with no control.
fn list_row_base(
    ui: &mut egui::Ui,
    app_id: u32,
    preferred_thumb: Option<&str>,
    title: &str,
    meta: &str,
    meta_color: Color32,
    right_w: f32,
) -> (egui::Rect, egui::Response) {
    // The card fills the top of its slot; the caller wraps each row in a fixed-height region that
    // reserves LIST_ROW_GAP below, so a visible gap always separates the cards.
    let (rect, hover_resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), LIST_ROW_HEIGHT), Sense::hover());
    let hover = ui.ctx().animate_bool(hover_resp.id, hover_resp.hovered());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        lerp_color(SURFACE, SURFACE_RAISED, 0.35 + hover * 0.65),
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
        egui::StrokeKind::Inside,
    );
    // Small landscape thumbnail, vertically centred. A caller-supplied URL (the live feed's own,
    // already-valid capsule) is tried first, then the App-ID-derived CDN fallbacks.
    let th = LIST_ROW_HEIGHT - 16.0;
    let tw = th / STEAM_HEADER_ASPECT;
    let thumb = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 9.0, rect.center().y - th / 2.0),
        Vec2::new(tw, th),
    );
    let fallback = steam_artwork_urls(app_id);
    let mut refs: Vec<&str> = Vec::with_capacity(5);
    if let Some(url) = preferred_thumb.filter(|url| url.starts_with("https://")) {
        refs.push(url);
    }
    refs.extend(fallback.iter().map(String::as_str));
    // Cover-fit so the thumbnail fills its rect with no letterbox bars — the feed's capsule art is a
    // wider aspect than the header-shaped slot, which a plain fit would bar top and bottom.
    paint_remote_image_cover_multi(ui, thumb, &refs, egui::CornerRadius::same(4));

    let text_x = thumb.right() + 16.0;
    let painter = ui.painter().with_clip_rect(rect);
    let has_meta = !meta.is_empty();
    let title_y = if has_meta {
        rect.center().y - 9.0
    } else {
        rect.center().y
    };
    painter.text(
        egui::pos2(text_x, title_y),
        egui::Align2::LEFT_CENTER,
        title,
        FontId::proportional(15.0),
        TEXT,
    );
    if has_meta {
        painter.text(
            egui::pos2(text_x, rect.center().y + 11.0),
            egui::Align2::LEFT_CENTER,
            meta,
            FontId::proportional(11.0),
            meta_color,
        );
    }

    let right_zone = egui::Rect::from_min_size(
        egui::pos2(rect.right() - right_w - 12.0, rect.center().y - 16.0),
        Vec2::new(right_w, 32.0),
    );
    // The clickable area is everything left of the control (so the control gets its own clicks).
    let click_right = if right_w > 0.0 {
        right_zone.left() - 8.0
    } else {
        rect.right()
    };
    let click_rect = egui::Rect::from_min_max(rect.min, egui::pos2(click_right, rect.bottom()));
    let click = ui.interact(click_rect, hover_resp.id.with("click"), Sense::click());
    if click.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (right_zone, click)
}

/// A Steam-style store list row: thumbnail + title, an "Activatable in Drydock" meta line when Drydock
/// supports it, and a right-hand Activate/View control. No price (the store list is price-free).
fn store_list_row(ui: &mut egui::Ui, capsule: &StoreCapsule, activatable: bool) -> Option<StoreAction> {
    let (meta, meta_color) = if activatable {
        ("Activatable in Drydock", ACCENT_SOFT)
    } else {
        ("", MUTED)
    };
    let (label, primary, btn_w) = if activatable {
        ("ACTIVATE", true, 116.0)
    } else {
        ("VIEW", false, 84.0)
    };
    let mut action = None;
    // A fixed-height slot for the card, then an explicit gap below it. egui shrinks a nested
    // `allocate_ui` back to its content, so the gap can't be baked into the slot height — it has to
    // be added as real space after the row for the cards to separate.
    let width = ui.available_width();
    list_row_slot(ui, width, |ui| {
        let (right_zone, click) = list_row_base(
            ui,
            capsule.app_id,
            Some(capsule.header_image_url.as_str()),
            &capsule.name,
            meta,
            meta_color,
            btn_w,
        );
        if click.clicked() {
            action = Some(StoreAction::Details(capsule.app_id));
        }
        let button = if primary {
            success_button(label).min_size(Vec2::new(btn_w, 32.0))
        } else {
            ghost_button(label).min_size(Vec2::new(btn_w, 32.0))
        };
        if ui.put(right_zone, button).clicked() {
            action = Some(if activatable {
                StoreAction::Activate(capsule.app_id)
            } else {
                StoreAction::Details(capsule.app_id)
            });
        }
    });
    // The real gap between cards (the slot itself shrinks to the card, so this must be explicit).
    ui.add_space(LIST_ROW_GAP);
    action
}

/// Runs `contents` inside a fixed-height row slot (`LIST_ROW_HEIGHT`); the caller adds the gap below.
fn list_row_slot(ui: &mut egui::Ui, width: f32, contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, LIST_ROW_HEIGHT),
        Layout::top_down(Align::Min),
        |ui| {
            ui.set_width(width);
            contents(ui);
        },
    );
}

/// A successfully detected "Add game to Drydock" result: the game, its resolved install root and the
/// launch `.exe` found inside it.
struct AddGameOutcome {
    app_id: u32,
    name: String,
    root: PathBuf,
    exe: PathBuf,
}

/// Where a Library game comes from — decides its rail group and which management buttons it gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LibrarySource {
    /// Steam has it installed (a Steam `appmanifest.acf`).
    SteamInstalled,
    /// Drydock downloaded it through its own depot engine.
    DrydockInstalled,
    /// Its unlock Lua is in Steam, but the game itself isn't installed yet.
    Available,
}

/// One row of the Library page: a game the user owns, with enough state to render its Play button.
struct LibraryEntry {
    app_id: u32,
    name: String,
    /// Steam has this game installed (it launches through `steam://`).
    installed: bool,
    /// A remembered `.exe` for a game activated outside Steam, launched directly.
    launch_path: Option<String>,
    /// Which group/buttons this game belongs to.
    source: LibrarySource,
}

/// What a Library card wants the page to do once the frame is laid out (applied after the borrow of
/// the entries list ends).
enum LibraryAction {
    /// Highlight this game in the rail (right-hand overview switches to it).
    Select(u32),
    Details(u32),
    Launch(u32),
    SetExe(u32),
    /// Ask Steam to install a game whose Lua is already in place (`steam://install`).
    InstallSteam(u32),
    /// Ask Steam to uninstall a Steam-installed game (`steam://uninstall`).
    UninstallSteam(u32),
    /// Re-fetch and re-install the game's unlock Lua (latest version).
    UpdateLua(u32),
    /// Delete the game's unlock Lua from Steam (confirmed first).
    RemoveLua(u32),
    /// Verify a Drydock-downloaded game's files against its depot manifests.
    VerifyDrydock(u32),
    /// Re-download a Drydock game's changed/missing chunks from fresh depot data.
    UpdateDrydock(u32),
    /// Run the emu crack flow, deploying it straight into a Drydock game's install folder.
    CrackDrydock(u32),
    /// Delete a Drydock-downloaded game's install folder (confirmed first).
    UninstallDrydock(u32),
}

/// Height of one game row in the Library rail.
const RAIL_ROW_H: f32 = 44.0;

/// One collapsible Library rail group (e.g. "Installed in Steam"): a header with the game count and,
/// when expanded, the group's rows. Always rendered (even empty), open by default. Returns the App ID
/// of a row the user clicked, if any.
fn library_rail_group(
    ui: &mut egui::Ui,
    title: &str,
    source: LibrarySource,
    entries: &[LibraryEntry],
    selected: Option<u32>,
) -> Option<u32> {
    let group: Vec<&LibraryEntry> = entries.iter().filter(|entry| entry.source == source).collect();
    let mut clicked = None;
    let header = format!("{title}  ({})", group.len());
    egui::CollapsingHeader::new(RichText::new(header).size(12.0).strong().color(TEXT))
        .id_salt(("library_group", title))
        .default_open(true)
        .show(ui, |ui| {
            if group.is_empty() {
                ui.add_space(2.0);
                ui.label(RichText::new("No games here yet.").size(11.0).color(MUTED));
                ui.add_space(2.0);
                return;
            }
            for entry in group {
                if library_rail_row(ui, entry, selected == Some(entry.app_id)) {
                    clicked = Some(entry.app_id);
                }
            }
        });
    ui.add_space(4.0);
    clicked
}

/// One entry in the Steam-style Library rail: a small landscape thumbnail + title. Games that exist
/// only through Drydock' lua/manifest activation (not installed by Steam) are dimmed grey; the
/// highlighted game gets a violet fill and accent bar. Returns true when clicked.
fn library_rail_row(ui: &mut egui::Ui, entry: &LibraryEntry, selected: bool) -> bool {
    let dim = !entry.installed;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), RAIL_ROW_H), Sense::click());
    let hover = ui.ctx().animate_bool(resp.id, resp.hovered());
    let fill = if selected {
        lerp_color(SIDEBAR_FILL, ACCENT_DEEP, 0.40)
    } else {
        lerp_color(SIDEBAR_FILL, SURFACE_RAISED, hover)
    };
    ui.painter().rect_filled(rect, egui::CornerRadius::same(6), fill);
    if selected {
        let bar = egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
        ui.painter().rect_filled(bar, egui::CornerRadius::same(2), ACCENT);
    }
    let th = RAIL_ROW_H - 16.0;
    let tw = th / STEAM_HEADER_ASPECT;
    let thumb = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 9.0, rect.center().y - th / 2.0),
        Vec2::new(tw, th),
    );
    let urls = steam_artwork_urls(entry.app_id);
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    // Cover-fit so the thumbnail fills its rect with no letterbox bars.
    paint_remote_image_cover_multi(ui, thumb, &refs, egui::CornerRadius::same(3));
    let color = if dim && !selected { MUTED } else { TEXT };
    ui.painter().with_clip_rect(rect).text(
        egui::pos2(thumb.right() + 11.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &entry.name,
        FontId::proportional(13.0),
        color,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// The right-hand Library overview of the highlighted game: a full-width header banner, the title,
/// an Installed/Activated status line, and the Play / Set-.exe / Store-page controls.
fn library_overview(ui: &mut egui::Ui, entry: &LibraryEntry, body_h: f32) -> Option<LibraryAction> {
    let mut action = None;
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(14)
        .inner_margin(0)
        .show(ui, |ui| {
            ui.set_min_height(body_h - 2.0);
            let width = ui.available_width();
            let hero_h = (width * STEAM_HEADER_ASPECT).min(body_h * 0.5);
            let (hero, _) = ui.allocate_exact_size(Vec2::new(width, hero_h), Sense::hover());
            // Prefer the wide `library_hero.jpg` (built for banners), then the header/capsules, and
            // cover-fit (fill + crop) so the art fills the banner edge to edge — no letterbox bars,
            // whatever the source image's aspect ratio.
            let urls = [
                format!(
                    "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
                    entry.app_id
                ),
                format!(
                    "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
                    entry.app_id
                ),
            ];
            let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
            paint_remote_image_cover_multi(
                ui,
                hero,
                &refs,
                egui::CornerRadius {
                    nw: 14,
                    ne: 14,
                    sw: 0,
                    se: 0,
                },
            );
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(22, 18))
                .show(ui, |ui| {
                    ui.label(RichText::new(&entry.name).size(22.0).strong().color(TEXT));
                    ui.add_space(8.0);
                    let amber = AMBER;
                    let (status, color) = match entry.source {
                        LibrarySource::SteamInstalled => ("Installed in Steam — ready to play", VERDIGRIS),
                        LibrarySource::DrydockInstalled if entry.launch_path.is_some() => {
                            ("Installed by Drydock · launches from a linked .exe", ACCENT_SOFT)
                        }
                        LibrarySource::DrydockInstalled => {
                            ("Installed by Drydock · link a launcher to play", amber)
                        }
                        LibrarySource::Available => ("Lua ready in Steam · not installed yet", amber),
                    };
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, color);
                        ui.add_space(5.0);
                        ui.label(RichText::new(status).size(12.5).color(color));
                    });
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(format!("App ID {}", entry.app_id))
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(20.0);
                    let bh = 42.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        let mut click = |a: LibraryAction| action = Some(a);
                        match entry.source {
                            LibrarySource::Available => {
                                if ui
                                    .add(primary_button("INSTALL").min_size(Vec2::new(150.0, bh)))
                                    .clicked()
                                {
                                    click(LibraryAction::InstallSteam(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UPDATE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Re-fetch and re-install the unlock Lua")
                                    .clicked()
                                {
                                    click(LibraryAction::UpdateLua(entry.app_id));
                                }
                            }
                            LibrarySource::SteamInstalled => {
                                if ui
                                    .add(success_button("▶  PLAY").min_size(Vec2::new(140.0, bh)))
                                    .clicked()
                                {
                                    click(LibraryAction::Launch(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UPDATE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Re-fetch and re-install the unlock Lua")
                                    .clicked()
                                {
                                    click(LibraryAction::UpdateLua(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UNINSTALL").min_size(Vec2::new(120.0, bh)))
                                    .on_hover_text("Ask Steam to uninstall the game")
                                    .clicked()
                                {
                                    click(LibraryAction::UninstallSteam(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("REMOVE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Delete the unlock Lua from Steam")
                                    .clicked()
                                {
                                    click(LibraryAction::RemoveLua(entry.app_id));
                                }
                            }
                            LibrarySource::DrydockInstalled => {
                                if entry.launch_path.is_some() {
                                    if ui
                                        .add(success_button("▶  PLAY").min_size(Vec2::new(140.0, bh)))
                                        .clicked()
                                    {
                                        click(LibraryAction::Launch(entry.app_id));
                                    }
                                } else if ui
                                    .add(primary_button("SET .EXE").min_size(Vec2::new(140.0, bh)))
                                    .on_hover_text("Link the game's .exe so PLAY can launch it")
                                    .clicked()
                                {
                                    click(LibraryAction::SetExe(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("VERIFY").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Verify the downloaded files against the depot manifests")
                                    .clicked()
                                {
                                    click(LibraryAction::VerifyDrydock(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UPDATE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Check the depot for updated files and download them")
                                    .clicked()
                                {
                                    click(LibraryAction::UpdateDrydock(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("CRACK").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text(
                                        "Generate and deploy the emu crack into this game's folder",
                                    )
                                    .clicked()
                                {
                                    click(LibraryAction::CrackDrydock(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UNINSTALL").min_size(Vec2::new(120.0, bh)))
                                    .on_hover_text("Delete the downloaded game folder")
                                    .clicked()
                                {
                                    click(LibraryAction::UninstallDrydock(entry.app_id));
                                }
                            }
                        }
                        if ui
                            .add(ghost_button("STORE PAGE").min_size(Vec2::new(130.0, bh)))
                            .clicked()
                        {
                            click(LibraryAction::Details(entry.app_id));
                        }
                    });
                });
        });
    action
}

/// Formats an integer with thin thousands separators (e.g. 182140 -> "182,140").
/// Runs a depot download or verify to completion on a background thread, forwarding progress ticks
/// through `sender`. Returns a human summary on success or an error message.
/// Where a generated crack is written.
enum EmuOutput {
    /// Deploy the files into this game install folder (root).
    Deploy(PathBuf),
    /// Save the files as a ZIP (with the game's exe-subfolder path structure) at this path.
    Zip(PathBuf),
}

/// The architecture choice for the emulator cracker.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EmuArch {
    /// Detect from Steam app-info (osarch / exe path), then the game exe's PE header for a deploy.
    Auto,
    X64,
    X86,
}

/// Guesses the architecture from a depot's file layout: a Windows exe/dll under a `win64`/`bin64`/
/// `x64` path means x64, `win32`/`x86` means x86. Used as an account-free fallback when Steam
/// app-info doesn't declare `osarch`.
fn arch_from_depot_paths(data: &DepotData) -> Option<PeArch> {
    for manifest in &data.manifests {
        for file in &manifest.files {
            let path = file.path.to_ascii_lowercase();
            if !path.ends_with(".exe") && !path.ends_with(".dll") {
                continue;
            }
            if path.contains("win64") || path.contains("bin64") || path.contains("x64") {
                return Some(PeArch::X64);
            }
            if path.contains("win32") || path.contains("bin32") || path.contains("x86") {
                return Some(PeArch::X86);
            }
        }
    }
    None
}

/// Builds a Cold Client Loader crack for `app_id`: resolves the account-free config (depots from the
/// manifest, DLCs + languages from the store, achievements from the proxy) and the architecture (from
/// `arch_choice`, else Steam app-info, else the game exe's PE header), ensures the emu toolchain is
/// cached, then deploys into the game folder or writes a ZIP. Returns a summary or an error message.
fn build_emu_crack(
    app_id: u32,
    output: &EmuOutput,
    arch_choice: EmuArch,
    loader_name: &str,
    include_reframework: bool,
    skeleton: Option<&Path>,
    cache_dir: &Path,
) -> Result<String, String> {
    // The game's Windows exes (relative paths) give the exe sub-folder (`Bin64\`) and the fallback
    // architecture (reading a real exe's PE header when Steam can't say).
    let exes = fetch_windows_executables(app_id).unwrap_or_default();
    let prefix = exes
        .first()
        .and_then(|exe| {
            let normalized = exe.replace('\\', "/");
            Path::new(&normalized)
                .parent()
                .map(|parent| parent.to_string_lossy().replace('/', "\\"))
        })
        .unwrap_or_default();

    let proxy = ProxyClient::new().map_err(|error| error.to_string())?;
    let data = DepotData::fetch(&proxy, app_id).map_err(|error| error.to_string())?;
    let mut depots: Vec<u32> = data.manifests.iter().map(|manifest| manifest.depot_id).collect();
    depots.sort_unstable();
    depots.dedup();

    // Architecture: an explicit choice wins; for Auto the real game exe on disk is the ground truth
    // (the loader proxy DLL is loaded by that exe, so it must match its bitness) — Steam app-info can
    // be misreported. So a deploy reads the launched exe's PE header first, falling back to Steam
    // app-info and then the depot manifest's file paths when the exe can't be read (e.g. a ZIP build).
    let arch = match arch_choice {
        EmuArch::X64 => PeArch::X64,
        EmuArch::X86 => PeArch::X86,
        EmuArch::Auto => {
            let from_exe = || {
                let EmuOutput::Deploy(folder) = output else {
                    return None;
                };
                let exe = exes.first()?;
                let path = drydock_core::join_within(folder, exe);
                detect_pe_arch(&std::fs::read(path).ok()?)
            };
            let from_steam = || {
                fetch_windows_arch(app_id)
                    .ok()
                    .flatten()
                    .map(|is64| if is64 { PeArch::X64 } else { PeArch::X86 })
            };
            let from_manifest = || arch_from_depot_paths(&data);
            from_exe().or_else(from_steam).or_else(from_manifest).ok_or_else(|| {
                "Could not auto-detect the architecture. Pick x64 or x86 in the Arch selector and try again."
                    .to_owned()
            })?
        }
    };

    // DLCs + supported languages from Steam's public store listing (no account).
    let (dlcs, languages) = SteamStoreClient::new(cache_dir)
        .and_then(|client| client.details(app_id))
        .map(|details| (details.dlc, details.languages))
        .unwrap_or_default();
    let achievements_json = proxy.app_schema(app_id).unwrap_or_else(|_| "[]".to_owned());
    let achievements_count = achievements_json.matches("\"name\"").count();

    let skeleton_bytes = match skeleton {
        Some(path) => Some(std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?),
        None => None,
    };

    // Files use the game's exe sub-folder as prefix, so a deploy lands next to the exe and a ZIP
    // extracts correctly at the game root.
    let input = EmuTemplateInput {
        app_id,
        depots: depots.clone(),
        dlcs: dlcs.clone(),
        languages,
        exe_dir: prefix.clone(),
        achievements_json,
    };
    let mut files = input
        .build_files(skeleton_bytes.as_deref())
        .map_err(|error| error.to_string())?;
    let config_count = files.len();

    // The emu DLLs for this architecture, at the same prefix as the config files. `ensure_toolchain`
    // re-downloads automatically if a cached DLL went missing (e.g. deleted by antivirus).
    let toolchain = ensure_toolchain(cache_dir, false).map_err(|error| error.to_string())?;
    let prefix_bs = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}\\")
    };
    for dll in toolchain_dlls(&toolchain, arch, loader_name) {
        let bytes = std::fs::read(&dll.source).map_err(|_| {
            format!(
                "Emu file {} is missing — your antivirus likely removed it. Add a Windows Security \
                 exclusion for the Drydock cache and game folders, then click RE-DOWNLOAD EMU FILES.",
                dll.deploy_name
            )
        })?;
        files.push((format!("{prefix_bs}{}", dll.deploy_name), bytes));
    }
    let dll_count = files.len() - config_count;

    // Static extras that complete the template like the reference tokenfiles: the gbe_fork overlay
    // sound, the generic x64 load_dlls stubs, and (opt-in) the REFramework nightly. `ss` is the
    // template's `steam_settings\` folder (already carries the exe-subfolder prefix).
    let ss = input.steam_settings_dir();
    if let Some(sound) = overlay_sound_bytes(cache_dir) {
        files.push((format!("{ss}sounds\\overlay_achievement_notification.wav"), sound));
    }
    for (rel, bytes) in load_dll_files(arch) {
        files.push((format!("{ss}{rel}"), bytes));
    }
    if include_reframework {
        let dll = fetch_reframework_dll(cache_dir, false).map_err(|error| error.to_string())?;
        files.push((format!("{prefix_bs}dinput8.dll"), dll));
    }

    // Achievement icon images, mirrored locally so the overlay shows them offline (best-effort).
    let images = fetch_achievement_images(&achievement_image_urls(&input.achievements_json));
    let image_count = images.len();
    for (name, bytes) in images {
        files.push((format!("{ss}image\\{name}"), bytes));
    }
    let extras_note = {
        let mut parts = Vec::new();
        if image_count > 0 {
            parts.push(format!("{image_count} achievement image(s)"));
        }
        if include_reframework {
            parts.push("REFramework".to_owned());
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" + {}", parts.join(" + "))
        }
    };

    match output {
        EmuOutput::Deploy(folder) => {
            for (relative, contents) in &files {
                // Skeleton-ZIP entries reach us as author-controlled relative paths, so every
                // segment goes through the shared safety filter — `..` and a bare `C:` alike would
                // otherwise write outside the game folder.
                let target = drydock_core::join_within(folder, relative);
                if target == *folder {
                    continue; // every segment was rejected — nothing sane to write
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| format!("{}: {error}", parent.display()))?;
                }
                std::fs::write(&target, contents)
                    .map_err(|error| format!("{}: {error}", target.display()))?;
            }
            let folder_name = folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.display().to_string());
            Ok(format!(
                "Cracked App {app_id} ({}) into {folder_name} — {config_count} configs + \
                 {dll_count} DLLs{extras_note}, {achievements_count} achievement(s).",
                arch.folder(),
            ))
        }
        EmuOutput::Zip(zip_path) => {
            let bytes = drydock_core::zip_files(&files).map_err(|error| error.to_string())?;
            std::fs::write(zip_path, &bytes).map_err(|error| format!("{}: {error}", zip_path.display()))?;
            Ok(format!(
                "Crack ZIP for App {app_id} ({}) saved to {} — {config_count} config files + \
                 {dll_count} DLLs{extras_note}. {} depot(s), {} DLC(s), {achievements_count} achievement(s).",
                arch.folder(),
                zip_path.display(),
                depots.len(),
                dlcs.len()
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_depot_job(
    app_id: u32,
    name: &str,
    kind: DownloadKind,
    steam_root: Option<PathBuf>,
    installed_dir: Option<PathBuf>,
    connections: usize,
    max_bps: Option<u64>,
    cancel: &AtomicBool,
    sender: &mpsc::Sender<DownloadUpdate>,
) -> Result<String, String> {
    let proxy = ProxyClient::new().map_err(|error| error.to_string())?;
    let data = DepotData::fetch(&proxy, app_id).map_err(|error| error.to_string())?;

    // Refuse to "succeed" on a package that has no game content. Some upstream builds ship only the
    // shared redistributables (Visual C++, DirectX, …) with keys for the real content depots but no
    // manifest for them — downloading that would leave the game unplayable while claiming it
    // finished. Tell the user to retry once the source has packaged the content.
    if matches!(kind, DownloadKind::Download) && data.has_no_content() {
        return Err(format!(
            "{name} isn't fully available from the source yet: only the shared redistributables were \
             packaged (no game content depots). The upstream is likely still building the package — \
             try Download again in a few minutes."
        ));
    }

    let install_root = match installed_dir {
        Some(dir) => dir,
        None => {
            let root = steam_root.ok_or_else(|| "Steam folder not found. Set it in Settings.".to_owned())?;
            let installdir = fetch_install_dir(app_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Steam did not report an install folder for this game.".to_owned())?;
            root.join("steamapps").join("common").join(installdir)
        }
    };

    let forward = |progress: DownloadProgress| {
        let _ = sender.send(DownloadUpdate::Progress(progress));
    };
    match kind {
        DownloadKind::Download => {
            let cdn = CdnClient::new().map_err(|error| error.to_string())?;
            let outcome =
                depot::download::download(&data, &install_root, &cdn, cancel, connections, max_bps, forward)
                    .map_err(|error| error.to_string())?;
            Ok(format!(
                "Downloaded {name} — {} files, {}",
                outcome.files_written,
                human_bytes(outcome.bytes_written)
            ))
        }
        DownloadKind::Verify => {
            let outcome = depot::download::verify(&data, &install_root, cancel, forward)
                .map_err(|error| error.to_string())?;
            if outcome.is_complete() {
                Ok(format!(
                    "{name} verified — all {} chunks OK",
                    outcome.total_chunks
                ))
            } else {
                Ok(format!(
                    "{name}: {} of {} chunks need repair — press Download to fix",
                    outcome.bad_chunks, outcome.total_chunks
                ))
            }
        }
    }
}

/// Formats a byte-per-second rate as a compact human string (e.g. `9.9 MB/s`).
fn human_bps(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1.0 {
        return "0 B/s".to_owned();
    }
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut value = bytes_per_sec;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A compact Downloads-panel stat: an accent-coloured caption over its value.
fn download_mini_stat(ui: &mut egui::Ui, caption: &str, value: &str, accent: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(RichText::new(caption).size(9.0).strong().color(accent));
        ui.label(RichText::new(value).size(15.0).strong().color(TEXT));
    });
}

/// Keeps only the last `max` characters of `text`, prefixing an ellipsis when it was truncated.
fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let start = count - max;
    let mut out = String::from("…");
    out.extend(text.chars().skip(start));
    out
}

/// Truncates `text` to at most `max` characters, appending an ellipsis when it was cut (respecting
/// char boundaries). Used to keep status-bar messages from overrunning the bar.
fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && (bytes.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

/// The blocking Ubisoft prepare sequence (runs on a background thread): resolve the game exe via
/// Steam, download + install the magicfiles beside it, launch it once, capture the generated
/// token_req.txt, and mint the machine/App-bound activation code.
fn prepare_ubisoft(app_id: u32, chosen: &Path, settings_directory: &Path) -> Result<UbisoftPrepared, String> {
    if !chosen.is_dir() {
        return Err("Select the game's folder first.".to_owned());
    }
    let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
    let Some(exe_relative) = executables.first() else {
        return Err("Steam lists no launch executable for this game to verify against.".to_owned());
    };
    let root = resolve_game_root(chosen, &executables).ok_or_else(|| {
        "These files don't look like the selected game. Pick the correct game folder.".to_owned()
    })?;
    let exe = root.join(exe_relative);
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "Could not resolve the game executable's folder.".to_owned())?
        .to_path_buf();

    let magic = ProxyClient::new()
        .and_then(|client| client.magicfiles(app_id))
        .map_err(|error| error.to_string())?;
    install_magicfiles(&magic, &exe_dir).map_err(|error| error.to_string())?;
    clear_previous_token_files(&exe_dir);
    let token_request =
        run_and_capture_token_request(&exe, Duration::from_secs(180)).map_err(|error| error.to_string())?;
    let activation_code = ActivationRequestService::new(settings_directory)
        .and_then(|service| service.generate_ubisoft_delivery_code(app_id, &token_request))
        .map_err(|error| error.to_string())?;
    Ok(UbisoftPrepared {
        activation_code,
        exe_dir,
    })
}

/// One segment of the Activation page's STEAM/UBISOFT/EA switcher. Returns true when clicked.
fn activation_segment(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(108.0, 34.0), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        ACCENT_DEEP
    } else {
        lerp_color(SURFACE, SURFACE_RAISED, hover)
    };
    let stroke = if active {
        Stroke::NONE
    } else {
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover))
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        if active { Color32::WHITE } else { TEXT },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// The EA provider body — a placeholder until EA activation ships.
fn activation_ea_body(ui: &mut egui::Ui) {
    panel(ui, |ui| {
        section_label(ui, "EA ACTIVATION");
        ui.add_space(8.0);
        ui.label(
            RichText::new("EA activation is coming soon.")
                .size(13.0)
                .strong()
                .color(TEXT),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("This provider isn't available yet — use STEAM or UBISOFT for now.")
                .size(10.5)
                .color(MUTED),
        );
    });
}

fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(10.0).strong().color(ACCENT));
}

fn panel(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(16)
        .inner_margin(22)
        .show(ui, |ui| {
            // Fill the container width so stacked panels line up instead of shrinking to content.
            ui.set_width(ui.available_width());
            content(ui);
        });
}

fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

/// A small rounded status chip with a translucent tint of `color`.
/// A small rounded chip. `warning` prepends a drawn triangle. Both variants share the exact
/// same structure so status and activation pills always render at the same height.
fn pill(ui: &mut egui::Ui, text: &str, color: Color32, warning: bool) {
    egui::Frame::new()
        .fill(lerp_color(SURFACE, color, 0.18))
        .stroke(Stroke::new(1.0, lerp_color(BORDER, color, 0.55)))
        .corner_radius(255)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if warning {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(11.0, 10.0), Sense::hover());
                    draw_warning_triangle(ui.painter(), rect, color);
                }
                ui.label(RichText::new(text).size(9.5).strong().color(color));
            });
        });
}

fn status_pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    pill(ui, text, color, false);
}

/// An amber "activation needed" chip with a drawn warning triangle, for DRM-protected games.
/// Paints a small warning triangle with an exclamation mark inside `rect`.
fn draw_warning_triangle(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = Stroke::new(1.3, color);
    let top = egui::pos2(rect.center().x, rect.top());
    let bottom_left = egui::pos2(rect.left(), rect.bottom());
    let bottom_right = egui::pos2(rect.right(), rect.bottom());
    painter.add(egui::Shape::closed_line(
        vec![top, bottom_left, bottom_right],
        stroke,
    ));
    let cx = rect.center().x;
    painter.line_segment(
        [
            egui::pos2(cx, rect.top() + rect.height() * 0.38),
            egui::pos2(cx, rect.top() + rect.height() * 0.66),
        ],
        stroke,
    );
    painter.circle_filled(egui::pos2(cx, rect.bottom() - rect.height() * 0.13), 0.9, color);
}

/// A modern sliding toggle switch. Returns a response whose `changed()` fires on flip.
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool, accent: Color32) -> egui::Response {
    let size = Vec2::new(40.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, ""));

    let how_on = ui.ctx().animate_bool(response.id, *on);
    let painter = ui.painter();
    let radius = rect.height() / 2.0;
    let track = lerp_color(BORDER, accent, how_on);
    painter.rect_filled(rect, radius, track);
    let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
    painter.circle_filled(egui::pos2(knob_x, rect.center().y), radius - 3.0, Color32::WHITE);
    response
}

/// Which sidebar Steam Service button the user clicked this frame.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SteamServiceCardAction {
    None,
    Install,
    Uninstall,
    Reinstall,
    Restart,
}

/// A compact bottom-left status card: a coloured accent dot with a title and optional detail.
struct Notification {
    accent: Color32,
    title: String,
    detail: String,
}

impl Notification {
    fn error(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: DANGER,
            title: title.into(),
            detail: detail.into(),
        }
    }

    fn warn(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: AMBER,
            title: title.into(),
            detail: detail.into(),
        }
    }

    fn ok(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: ACCENT_SOFT,
            title: title.into(),
            detail: detail.into(),
        }
    }

    fn info(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: ACCENT,
            title: title.into(),
            detail: detail.into(),
        }
    }
}

fn steam_service_card(
    ui: &mut egui::Ui,
    steam: &SteamDiscovery,
    status: Option<&SteamServiceStatus>,
    service_busy: bool,
    restart_enabled: bool,
) -> SteamServiceCardAction {
    let mut action = SteamServiceCardAction::None;
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(14)
        .show(ui, |ui| {
            section_label(ui, "STEAM SERVICE");
            let Some(root) = &steam.root else {
                ui.label(RichText::new("STEAM NOT FOUND").size(15.0).strong().color(DANGER));
                ui.label(
                    RichText::new("Set the Steam folder in Settings.")
                        .size(8.5)
                        .color(MUTED),
                );
                return;
            };

            let (state_label, color, message, installed) = match status {
                Some(status) => {
                    let (label, color) = match status.state {
                        SteamServiceState::Current => ("CURRENT", ACCENT_SOFT),
                        SteamServiceState::UpdateAvailable => ("UPDATE AVAILABLE", ACCENT),
                        SteamServiceState::NotInstalled => ("NOT INSTALLED", MUTED),
                        SteamServiceState::Error => ("ATTENTION", DANGER),
                    };
                    let installed = status.state != SteamServiceState::NotInstalled;
                    (label, color, status.message.clone(), installed)
                }
                None if service_busy => ("CHECKING…", MUTED, String::new(), false),
                None => ("UNKNOWN", MUTED, String::new(), false),
            };
            ui.add_space(6.0);
            status_pill(ui, state_label, color);
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(RichText::new(root.display().to_string()).size(8.5).color(MUTED)).truncate(),
            );
            if !message.is_empty() {
                ui.add(egui::Label::new(RichText::new(message).size(9.0).color(MUTED)).wrap());
            }
            ui.add_space(10.0);

            let full = ui.available_width();
            let is_current = matches!(
                status.map(|status| status.state),
                Some(SteamServiceState::Current)
            );
            // The primary button carries the state action (Install/Update/Repair). When the
            // service is already current there is nothing to install, so it is hidden and the
            // Reinstall/Uninstall pair below takes over.
            if !is_current {
                let primary_label = status
                    .map_or("INSTALL", SteamServiceStatus::action_text)
                    .to_uppercase();
                if ui
                    .add_enabled(
                        !service_busy,
                        primary_button(&primary_label).min_size(Vec2::new(full, 34.0)),
                    )
                    .clicked()
                {
                    action = SteamServiceCardAction::Install;
                }
                if installed {
                    ui.add_space(6.0);
                }
            }
            // Reinstall and uninstall only make sense once the service is installed. They are
            // stacked full-width so their labels never overflow the narrow sidebar card.
            if installed {
                if ui
                    .add_enabled(
                        !service_busy,
                        ghost_button("REINSTALL").min_size(Vec2::new(full, 30.0)),
                    )
                    .on_hover_text("Download and reinstall the Steam Service files")
                    .clicked()
                {
                    action = SteamServiceCardAction::Reinstall;
                }
                ui.add_space(6.0);
                if ui
                    .add_enabled(
                        !service_busy,
                        ghost_button("UNINSTALL").min_size(Vec2::new(full, 30.0)),
                    )
                    .on_hover_text("Remove the Steam Service files and restart Steam")
                    .clicked()
                {
                    action = SteamServiceCardAction::Uninstall;
                }
            }
            ui.add_space(6.0);
            if ui
                .add_enabled(
                    restart_enabled && !service_busy,
                    ghost_button("RESTART STEAM").min_size(Vec2::new(full, 30.0)),
                )
                .on_hover_text("Stop and restart the Steam client")
                .clicked()
            {
                action = SteamServiceCardAction::Restart;
            }
        });
    action
}

/// A segmented-control tab for switching How It Works walkthroughs.
fn guide_tab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(11.0);
    let width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width + 40.0, 34.0), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        ACCENT_DEEP
    } else {
        lerp_color(Color32::TRANSPARENT, SURFACE_RAISED, hover)
    };
    let text_color = if active {
        Color32::WHITE
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    ui.painter().rect_filled(rect, 8, fill);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, text_color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, text_color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// One stage of the How It Works guide: a gradient index badge with a title and subtitle, then
/// its numbered steps laid out as a connected vertical timeline. `step_number` continues across
/// stages so the steps read 1..N over the whole flow.
fn guide_stage(
    ui: &mut egui::Ui,
    index: usize,
    title: &str,
    subtitle: &str,
    steps: &[(&str, &str)],
    step_number: &mut usize,
) {
    panel(ui, |ui| {
        // Header: a rounded gradient badge carrying the stage index, then the stage title.
        ui.horizontal(|ui| {
            let (badge, _) = ui.allocate_exact_size(Vec2::splat(42.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(badge, 12, ACCENT_DEEP);
            // A soft top highlight fakes a vertical gradient on the badge.
            painter.rect_filled(
                egui::Rect::from_min_size(badge.min, Vec2::new(42.0, 21.0)),
                egui::CornerRadius {
                    nw: 12,
                    ne: 12,
                    sw: 0,
                    se: 0,
                },
                Color32::from_rgba_unmultiplied(255, 255, 255, 24),
            );
            painter.text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                format!("{index:02}"),
                FontId::proportional(17.0),
                Color32::WHITE,
            );
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
                ui.label(RichText::new(subtitle).size(10.5).color(ACCENT));
            });
        });
        ui.add_space(16.0);

        // Steps as a timeline: the connecting rail is drawn behind the numbered nodes so it reads
        // as one continuous flow. Node centres are collected during layout, then painted after.
        let rail_width = 34.0;
        let node_radius = 11.0;
        let mut nodes: Vec<f32> = Vec::new();
        let mut rail_x = 0.0_f32;
        ui.vertical(|ui| {
            for (offset, (step_title, detail)) in steps.iter().enumerate() {
                if offset > 0 {
                    ui.add_space(16.0);
                }
                ui.horizontal_top(|ui| {
                    rail_x = ui.cursor().min.x + rail_width / 2.0;
                    ui.add_space(rail_width);
                    let content = ui.vertical(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(*step_title).size(12.5).strong().color(TEXT))
                                .wrap(),
                        );
                        ui.add_space(2.0);
                        ui.add(egui::Label::new(RichText::new(*detail).size(10.5).color(MUTED)).wrap());
                    });
                    nodes.push(content.response.rect.top() + 9.0);
                });
            }
        });

        if let (Some(&first), Some(&last)) = (nodes.first(), nodes.last())
            && nodes.len() > 1
        {
            ui.painter().line_segment(
                [egui::pos2(rail_x, first), egui::pos2(rail_x, last)],
                Stroke::new(2.0, lerp_color(BORDER, ACCENT_DEEP, 0.5)),
            );
        }
        for &y in &nodes {
            let center = egui::pos2(rail_x, y);
            ui.painter()
                .circle_filled(center, node_radius, lerp_color(SURFACE, ACCENT_DEEP, 0.32));
            ui.painter().circle_stroke(
                center,
                node_radius,
                Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.6)),
            );
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                step_number.to_string(),
                FontId::proportional(10.5),
                TEXT,
            );
            *step_number += 1;
        }
    });
}

/// A stacked label/value field for the narrow details column.
fn detail_field(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).size(8.5).strong().color(ACCENT));
    ui.add(egui::Label::new(RichText::new(value).size(11.0).color(TEXT)).wrap());
    ui.add_space(10.0);
}

/// Renders the whole details body inside a fixed-width column. Returns whether the Steam
/// action was clicked and the (possibly advanced) screenshot index. Kept free of `self` so
/// it can run inside the centring layout closures without borrow conflicts.
/// Native-depot-download button state for the details sidebar.
#[derive(Clone, Copy)]
struct DepotButtons {
    /// The proxy has key + manifests for this app.
    downloadable: bool,
    /// The game is installed (so Verify makes sense).
    installed: bool,
    /// A download/verify job is currently running (buttons disabled).
    busy: bool,
}

#[allow(clippy::too_many_arguments)]
fn details_body(
    ui: &mut egui::Ui,
    details: &SteamStoreDetails,
    state: DetailsState,
    width: f32,
    shot_index: usize,
    activation_required: bool,
    panels: DetailsPanels,
    depot: DepotButtons,
) -> (DetailsAction, usize) {
    let gap = 20.0;
    let mut action = DetailsAction::None;
    let mut new_shot = shot_index;

    // The game title heads the page, Steam-store style.
    ui.add(egui::Label::new(RichText::new(&details.name).size(30.0).strong().color(TEXT)).wrap());
    ui.label(
        RichText::new(format!("APP {}", details.app_id))
            .size(10.0)
            .color(ACCENT),
    );
    ui.add_space(16.0);

    // Two flowing columns so neither side leaves a big empty gap: the left stacks the media carousel
    // over "About this game"; the right stacks the store sidebar over Features & DRM and the system
    // requirements. On narrow windows everything collapses into one column.
    if width >= 900.0 {
        let right_w = (width * 0.35).clamp(320.0, 440.0);
        let left_w = width - right_w - gap;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(left_w, 10.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left_w);
                new_shot = details_media(ui, details, shot_index);
                ui.add_space(24.0);
                details_about(ui, details);
            });
            ui.add_space(gap);
            ui.allocate_ui_with_layout(Vec2::new(right_w, 10.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(right_w);
                action = details_store_sidebar(ui, details, state, activation_required, panels, depot);
                ui.add_space(24.0);
                details_features(ui, details, activation_required);
                ui.add_space(24.0);
                details_requirements(ui, details);
            });
        });
    } else {
        new_shot = details_media(ui, details, shot_index);
        ui.add_space(16.0);
        action = details_store_sidebar(ui, details, state, activation_required, panels, depot);
        ui.add_space(24.0);
        details_about(ui, details);
        ui.add_space(24.0);
        details_features(ui, details, activation_required);
        ui.add_space(24.0);
        details_requirements(ui, details);
    }
    (action, new_shot)
}

/// The details media column: the screenshot carousel, or the header art when a game has no shots.
fn details_media(ui: &mut egui::Ui, details: &SteamStoreDetails, shot_index: usize) -> usize {
    if details.screenshots.is_empty() {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width * STEAM_HEADER_ASPECT), Sense::hover());
        paint_remote_image(
            ui,
            rect,
            &details.header_image_url,
            egui::CornerRadius::same(12),
            "ARTWORK UNAVAILABLE",
        );
        return shot_index;
    }
    screenshot_gallery(ui, &details.screenshots, shot_index)
}

/// The Steam-style store sidebar: the header capsule, the short description, the facts block
/// (reviews / release / developer / publisher), the tag chips, and the Drydock action buttons.
fn details_store_sidebar(
    ui: &mut egui::Ui,
    details: &SteamStoreDetails,
    state: DetailsState,
    activation_required: bool,
    panels: DetailsPanels,
    depot: DepotButtons,
) -> DetailsAction {
    let mut action = DetailsAction::None;
    panel(ui, |ui| {
        let inner = ui.available_width();
        if !details.header_image_url.is_empty() {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(inner, inner * STEAM_HEADER_ASPECT), Sense::hover());
            paint_remote_image(
                ui,
                rect,
                &details.header_image_url,
                egui::CornerRadius::same(8),
                "",
            );
            ui.add_space(12.0);
        }
        if !details.short_description.is_empty() {
            ui.add(egui::Label::new(RichText::new(&details.short_description).size(12.0).color(TEXT)).wrap());
            ui.add_space(14.0);
        }

        sidebar_fact(ui, "ALL REVIEWS", &details.reviews.display_text(), ACCENT_SOFT);
        sidebar_fact(ui, "RELEASED", &value_or_unknown(&details.release_date), TEXT);
        sidebar_fact(ui, "DEVELOPER", &joined_or_unknown(&details.developers), ACCENT);
        sidebar_fact(ui, "PUBLISHER", &joined_or_unknown(&details.publishers), ACCENT);
        if activation_required {
            let notice = details.drm_notice.trim();
            let text = if notice.is_empty() {
                "Denuvo — activation required".to_owned()
            } else {
                format!("{notice} — activation required")
            };
            sidebar_fact(ui, "DRM", &text, AMBER);
        }

        if !details.genres.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("TAGS").size(8.5).strong().color(MUTED));
            ui.add_space(6.0);
            tag_chip_flow(ui, &details.genres);
        }

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(12.0);
        // The Drydock action hub (Add to Steam / Activate / Apply Fix / Download).
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            action = steam_button_row(ui, state, panels, activation_required);
        });

        // Native depot download: real game files via manifest + key, shown only when the proxy has
        // download data for this app.
        if depot.downloadable {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                // Enabled even while another download runs — it just goes into the queue and starts
                // when the current one finishes (enqueue_download won't add the same game twice).
                let download_label = if depot.busy {
                    "ADD TO QUEUE"
                } else if depot.installed {
                    "DOWNLOAD / REPAIR"
                } else {
                    "DOWNLOAD IN DRYDOCK"
                };
                let download_hint = if depot.busy {
                    "Queue this game — it starts once the current download finishes"
                } else {
                    "Download the real game files into your Steam library (manifest + depot key → Steam CDN)"
                };
                if ui
                    .add(success_button(download_label))
                    .on_hover_text(download_hint)
                    .clicked()
                {
                    action = DetailsAction::DepotDownload;
                }
                if depot.installed
                    && ui
                        .add_enabled(!depot.busy, ghost_button("VERIFY FILES"))
                        .on_hover_text("Check the installed files against the depot manifest")
                        .clicked()
                {
                    action = DetailsAction::DepotVerify;
                }
            });
        }
    });
    action
}

/// One label + value line in the store sidebar, Steam-store style: a fixed-width right-aligned grey
/// label column and a value column that starts at the same x on every row and wraps if needed.
fn sidebar_fact(ui: &mut egui::Ui, label: &str, value: &str, value_color: Color32) {
    const LABEL_W: f32 = 96.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        // Left-aligned label column of a fixed width, so labels sit flush left (aligned with TAGS)
        // and all values still line up in one column.
        ui.allocate_ui_with_layout(Vec2::new(LABEL_W, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(LABEL_W);
            ui.add_space(1.0);
            ui.add(egui::Label::new(RichText::new(label).size(9.5).color(MUTED)));
        });
        ui.add(egui::Label::new(RichText::new(value).size(11.5).color(value_color)).wrap());
    });
    ui.add_space(8.0);
}

/// Lays out tag chips across as many rows as needed, breaking a row before a chip that would
/// overflow the available width. egui's `horizontal_wrapped` squeezes an over-wide Frame chip into
/// the leftover space (which then clips or char-wraps) instead of moving it to the next row, so the
/// row breaks are computed here from each chip's measured width.
fn tag_chip_flow(ui: &mut egui::Ui, tags: &[String]) {
    const GAP: f32 = 6.0;
    const CHIP_PAD: f32 = 9.0 * 2.0 + 2.0; // symmetric inner margin + stroke
    let avail = ui.available_width();

    let mut rows: Vec<Vec<&str>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for tag in tags.iter().take(14) {
        let galley = ui
            .painter()
            .layout_no_wrap(tag.clone(), FontId::proportional(10.5), TEXT);
        let w = galley.size().x + CHIP_PAD;
        let row = rows.last_mut().expect("one row always present");
        if !row.is_empty() && used + GAP + w > avail {
            rows.push(vec![tag.as_str()]);
            used = w;
        } else {
            if !row.is_empty() {
                used += GAP;
            }
            used += w;
            row.push(tag.as_str());
        }
    }

    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            ui.add_space(GAP);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for tag in row {
                tag_chip(ui, tag);
            }
        });
    }
}

/// A rounded genre/tag pill for the store sidebar.
fn tag_chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(lerp_color(SURFACE_RAISED, ACCENT_SOFT, 0.10))
        .stroke(Stroke::new(1.0, lerp_color(BORDER, ACCENT_SOFT, 0.30)))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            // Keep the chip on one line: never let the label wrap character-by-character. If it
            // doesn't fit the row, the flow layout moves the whole chip to the next line.
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .size(10.5)
                        .color(lerp_color(TEXT, ACCENT_SOFT, 0.4)),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

/// The features / DRM rail beside "About this game": platforms, Metacritic and the DRM notice.
fn details_features(ui: &mut egui::Ui, details: &SteamStoreDetails, activation_required: bool) {
    section_label(ui, "FEATURES & DRM");
    ui.add_space(8.0);
    panel(ui, |ui| {
        detail_field(ui, "PLATFORMS", joined_or_unknown(&details.platforms));
        detail_field(
            ui,
            "METACRITIC",
            details
                .metacritic_score
                .map_or_else(|| "Not rated".to_owned(), |score| format!("{score} / 100")),
        );
        let drm = if activation_required {
            let notice = details.drm_notice.trim();
            if notice.is_empty() {
                "Denuvo Anti-Tamper — Drydock activation required".to_owned()
            } else {
                format!("{notice} — Drydock activation required")
            }
        } else {
            "None — no activation needed".to_owned()
        };
        ui.label(RichText::new("THIRD-PARTY DRM").size(8.5).strong().color(ACCENT));
        ui.add(
            egui::Label::new(RichText::new(drm).size(11.0).color(if activation_required {
                AMBER
            } else {
                TEXT
            }))
            .wrap(),
        );
    });
}

/// A button the user pressed on an app's details page.
#[derive(Clone, Copy, Eq, PartialEq)]
enum DetailsAction {
    None,
    /// Add the normal unlock Lua so Steam installs/updates the game to its latest build.
    AddToSteam,
    /// Add the GitHub build-locked "Denuvo fix" Lua so the game is pinned to the cracked build.
    AddCracked,
    RemoveFromSteam,
    /// Install the Steam Service (shown in place of Add to Steam when it is not installed).
    InstallService,
    /// Apply the app's GitHub build-locked Denuvo fix.
    ApplyDenuvoFix,
    /// Open the repack source at this index (into the app's repack sources) in the browser.
    Download(usize),
    /// Jump to the Activation tab with this game preselected.
    Activate,
    /// Download the real game files via the native depot downloader (manifest + key → Steam CDN).
    DepotDownload,
    /// Verify the installed files against the depot manifest.
    DepotVerify,
}

/// Fix availability and button state for the app currently on the details page — a build-locked
/// Denuvo fix from the GitHub MFB repo (the DepotBox "online fix" API path was removed).
#[derive(Clone)]
struct FixPanelState {
    /// A background action (e.g. this fix, or an add-to-Steam) is running.
    busy: bool,
    /// The game is installed, so there is a folder to extract the fix into.
    installed: bool,
    /// The Denuvo fix's installed status, if the app has a Denuvo fix.
    denuvo: Option<FixStatus>,
}

/// The repacker names for the app on the details page, one Download button each. The link for each
/// is resolved by index from the app's repack sources when the button is clicked.
#[derive(Clone)]
struct RepackPanelState {
    repackers: Vec<String>,
}

/// The optional action panels shown on the details page: the Apply-Fix buttons and the repack
/// Download buttons. Bundled so the render helpers stay under the argument limit.
#[derive(Clone, Copy)]
struct DetailsPanels<'a> {
    fix: Option<&'a FixPanelState>,
    repack: Option<&'a RepackPanelState>,
}

/// The state that decides which app-page buttons are shown and enabled.
#[derive(Clone, Copy)]
struct DetailsState {
    /// The unlock Lua has been added to the Steam plug-in folder.
    is_added: bool,
    /// The Steam Service is current, so an app may be added.
    service_current: bool,
    /// A Steam Service background operation is in progress.
    busy: bool,
}

/// Renders the details-page action buttons (the hub for a game) and returns the one that was
/// clicked: add the latest or cracked unlock, apply the Denuvo fix, download a repack, or jump
/// to Activation.
fn steam_button_row(
    ui: &mut egui::Ui,
    state: DetailsState,
    panels: DetailsPanels,
    activation_required: bool,
) -> DetailsAction {
    let mut action = DetailsAction::None;
    let installed = panels.fix.is_some_and(|fix| fix.installed);
    let busy_fix = panels.fix.is_some_and(|fix| fix.busy);
    let has_denuvo = panels.fix.is_some_and(|fix| fix.denuvo.is_some());

    // Add-to-Steam workflow: latest and (when a Denuvo fix exists) cracked, or Update/Remove once added.
    if state.is_added {
        if ui
            .add_enabled(!state.busy, primary_button("UPDATE"))
            .on_hover_text("Re-fetch and re-install the latest unlock Lua")
            .clicked()
        {
            action = DetailsAction::AddToSteam;
        }
        if ui
            .add_enabled(!state.busy, ghost_button("REMOVE FROM STEAM"))
            .clicked()
        {
            action = DetailsAction::RemoveFromSteam;
        }
    } else if state.service_current {
        if ui
            .add_enabled(!state.busy, primary_button("ADD LATEST VERSION TO STEAM"))
            .on_hover_text("Add the normal unlock so Steam installs the latest build")
            .clicked()
        {
            action = DetailsAction::AddToSteam;
        }
        if has_denuvo
            && ui
                .add_enabled(!state.busy, primary_button("ADD CRACKED VERSION TO STEAM"))
                .on_hover_text("Add the build-locked unlock that pins the game to the cracked build")
                .clicked()
        {
            action = DetailsAction::AddCracked;
        }
    } else {
        // The Steam Service must be installed before any app can be added, so offer to install it
        // right here instead of just disabling the button.
        let response = ui
            .add_enabled(!state.busy, primary_button("INSTALL STEAM SERVICE"))
            .on_hover_text(
                "The Steam Service must be installed before adding games. Click to install it now.",
            );
        if response.clicked() {
            action = DetailsAction::InstallService;
        }
    }

    if let Some(fix) = panels.fix {
        // Denuvo fix (GitHub build-locked Lua + zip), labelled with its installed status.
        if let Some(status) = fix.denuvo {
            let text = match status {
                FixStatus::Applied => "RE-APPLY DENUVO FIX",
                FixStatus::IncompatibleLua | FixStatus::NotApplied => "APPLY DENUVO FIX",
            };
            let response = ui.add_enabled(installed && !busy_fix, ghost_button(text));
            let response = if !installed {
                response.on_hover_text("Install the game through Steam first.")
            } else if busy_fix {
                response.on_hover_text("A background action is already running.")
            } else {
                match status {
                    FixStatus::Applied => response.on_hover_text(
                        "The verified fix Lua is installed. Re-apply to refresh the game files.",
                    ),
                    FixStatus::IncompatibleLua => response.on_hover_text(
                        "A different Lua is installed for this app — applying replaces it with the verified fix Lua.",
                    ),
                    FixStatus::NotApplied => response,
                }
            };
            if response.clicked() {
                action = DetailsAction::ApplyDenuvoFix;
            }
        }
    }

    // Download the game as an external repack — one button per repacker source.
    if let Some(repack) = panels.repack {
        let single = repack.repackers.len() == 1;
        for (index, repacker) in repack.repackers.iter().enumerate() {
            let label = if single {
                "DOWNLOAD REPACK".to_owned()
            } else {
                format!("DOWNLOAD REPACK ({repacker})")
            };
            if ui
                .add(ghost_button(&label))
                .on_hover_text(format!("Open {repacker} in your browser"))
                .clicked()
            {
                action = DetailsAction::Download(index);
            }
        }
    }

    // Activate — only for Denuvo games; opens the Activation tab with this game preselected.
    if activation_required
        && ui
            .add(ghost_button("ACTIVATE"))
            .on_hover_text("Open Activation with this game preselected")
            .clicked()
    {
        action = DetailsAction::Activate;
    }
    action
}

fn details_about(ui: &mut egui::Ui, details: &SteamStoreDetails) {
    section_label(ui, "ABOUT THIS GAME");
    ui.add_space(8.0);
    panel(ui, |ui| {
        let empty = details.about_the_game.is_empty();
        // Show the full description at its natural height (no inner scrollbar); the page scrolls.
        ui.add(
            egui::Label::new(
                RichText::new(if empty {
                    "No description is available."
                } else {
                    &details.about_the_game
                })
                .size(12.0)
                .color(if empty { MUTED } else { TEXT }),
            )
            .wrap(),
        );
    });
}

fn details_requirements(ui: &mut egui::Ui, details: &SteamStoreDetails) {
    section_label(ui, "SYSTEM REQUIREMENTS");
    ui.add_space(8.0);
    // In the narrow right column the two side-by-side columns get cramped, so stack them there.
    let stacked = ui.available_width() < 520.0;
    panel(ui, |ui| {
        if stacked {
            requirement_column(ui, "MINIMUM", &details.requirements.minimum);
            ui.add_space(12.0);
            requirement_column(ui, "RECOMMENDED", &details.requirements.recommended);
        } else {
            ui.columns(2, |columns| {
                requirement_column(&mut columns[0], "MINIMUM", &details.requirements.minimum);
                requirement_column(&mut columns[1], "RECOMMENDED", &details.requirements.recommended);
            });
        }
    });
}

fn requirement_column(ui: &mut egui::Ui, heading: &str, value: &str) {
    ui.label(RichText::new(heading).size(10.0).strong().color(ACCENT));
    ui.add_space(6.0);
    ui.label(
        RichText::new(if value.is_empty() { "Not specified" } else { value })
            .size(10.5)
            .color(if value.is_empty() { MUTED } else { TEXT }),
    );
}

fn joined_or_unknown(values: &[String]) -> String {
    if values.is_empty() {
        "Not available".into()
    } else {
        values.join(", ")
    }
}

fn value_or_unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "Not available".into()
    } else {
        value.to_owned()
    }
}

fn response_code_fields(ui: &mut egui::Ui, characters: &mut [String; 8]) {
    let pasted = ui.input(|input| {
        input.events.iter().rev().find_map(|event| match event {
            egui::Event::Paste(value) => Some(value.clone()),
            _ => None,
        })
    });
    let focused = (0..characters.len()).find(|index| {
        ui.memory(|memory| memory.has_focus(egui::Id::new(("response_code_character", *index))))
    });
    if let (Some(value), Some(index)) = (pasted, focused) {
        let focus = distribute_response_code(characters, index, &value);
        ui.memory_mut(|memory| {
            memory.request_focus(egui::Id::new(("response_code_character", focus)));
        });
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for index in 0..characters.len() {
            let id = egui::Id::new(("response_code_character", index));
            let was_empty = characters[index].is_empty();
            let response = ui.add_sized(
                [42.0, 48.0],
                egui::TextEdit::singleline(&mut characters[index])
                    .id(id)
                    .char_limit(1)
                    .font(egui::TextStyle::Heading)
                    .horizontal_align(egui::Align::Center),
            );
            if response.changed() {
                characters[index] = normalize_response_fragment(&characters[index])
                    .chars()
                    .next()
                    .map_or_else(String::new, |character| character.to_string());
                if !characters[index].is_empty() && index + 1 < characters.len() {
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(("response_code_character", index + 1)));
                    });
                }
            }
            if response.has_focus() {
                let move_left = ui.input(|input| input.key_pressed(egui::Key::ArrowLeft));
                let move_right = ui.input(|input| input.key_pressed(egui::Key::ArrowRight));
                let back_to_previous = was_empty && ui.input(|input| input.key_pressed(egui::Key::Backspace));
                if (move_left || back_to_previous) && index > 0 {
                    if back_to_previous {
                        characters[index - 1].clear();
                    }
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(("response_code_character", index - 1)));
                    });
                } else if move_right && index + 1 < characters.len() {
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(("response_code_character", index + 1)));
                    });
                }
            }
        }
    });
}

fn is_short_activation_code(value: &str) -> bool {
    value.len() == 8 && value.chars().all(|character| character.is_ascii_alphanumeric())
}

fn normalize_response_fragment(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

fn distribute_response_code(characters: &mut [String; 8], start: usize, value: &str) -> usize {
    let normalized = normalize_response_fragment(value);
    let mut next = start.min(characters.len() - 1);
    for (offset, character) in normalized.chars().enumerate() {
        let index = start + offset;
        if index >= characters.len() {
            break;
        }
        characters[index] = character.to_string();
        next = (index + 1).min(characters.len() - 1);
    }
    next
}

/// Paints a remote image into `rect`. Returns true if the image failed to load, so callers
/// can try to resolve a better URL.
fn paint_remote_image(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    uri: &str,
    corner: egui::CornerRadius,
    fallback: &str,
) -> bool {
    ui.painter().rect_filled(rect, corner, SURFACE);
    let texture = ui.ctx().try_load_texture(
        uri,
        egui::TextureOptions::LINEAR,
        egui::load::SizeHint::Width(rect.width().max(1.0) as u32),
    );
    match texture {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            ui.put(
                rect,
                egui::Image::from_texture(texture)
                    .fit_to_exact_size(rect.size())
                    .corner_radius(corner),
            );
            false
        }
        Ok(egui::load::TexturePoll::Pending { .. }) => {
            // No spinner — the neutral tile stays until the art loads in the background.
            false
        }
        Err(_) => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                fallback,
                FontId::monospace(11.0),
                MUTED,
            );
            true
        }
    }
}

/// Paints a remote image so it *covers* `rect` — scaled up until it fills the whole rectangle, with
/// any overflow cropped (via a centred UV window) rather than letterboxed. Keeps the rounded corners.
/// Used for the screenshot carousel so the preview never shows side/top bars regardless of the
/// image's exact aspect ratio.
fn paint_remote_image_cover(ui: &mut egui::Ui, rect: egui::Rect, uri: &str, corner: egui::CornerRadius) {
    ui.painter().rect_filled(rect, corner, SURFACE);
    let texture = ui.ctx().try_load_texture(
        uri,
        egui::TextureOptions::LINEAR,
        egui::load::SizeHint::Width(rect.width().max(1.0) as u32),
    );
    match texture {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            let img = texture.size;
            let img_aspect = if img.y > 0.0 { img.x / img.y } else { 1.0 };
            let rect_aspect = rect.height().max(1.0).recip() * rect.width();
            // Crop the dimension that would otherwise overflow, centred, so the visible window's
            // aspect matches the rect exactly — no distortion, no bars.
            let uv = if rect_aspect > img_aspect {
                let h = (img_aspect / rect_aspect).clamp(0.0, 1.0);
                egui::Rect::from_min_max(egui::pos2(0.0, (1.0 - h) / 2.0), egui::pos2(1.0, (1.0 + h) / 2.0))
            } else {
                let w = (rect_aspect / img_aspect).clamp(0.0, 1.0);
                egui::Rect::from_min_max(egui::pos2((1.0 - w) / 2.0, 0.0), egui::pos2((1.0 + w) / 2.0, 1.0))
            };
            ui.put(
                rect,
                egui::Image::from_texture(texture)
                    .uv(uv)
                    .fit_to_exact_size(rect.size())
                    .maintain_aspect_ratio(false)
                    .corner_radius(corner),
            );
        }
        Ok(egui::load::TexturePoll::Pending { .. }) => {
            // No spinner — the neutral tile stays until the screenshot loads in the background.
        }
        Err(_) => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "SCREENSHOT UNAVAILABLE",
                FontId::monospace(11.0),
                MUTED,
            );
        }
    }
}

/// Like [`paint_remote_image_cover`] but tries several URIs in order, advancing to the next only when
/// one definitively fails to load. Cover-fills the rect (no letterbox bars) — used for the Downloads
/// banner, where the card is much wider than the source art.
fn paint_remote_image_cover_multi(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    uris: &[&str],
    corner: egui::CornerRadius,
) -> bool {
    ui.painter().rect_filled(rect, corner, SURFACE);
    for (index, uri) in uris.iter().enumerate() {
        let is_last = index + 1 == uris.len();
        let texture = ui.ctx().try_load_texture(
            uri,
            egui::TextureOptions::LINEAR,
            egui::load::SizeHint::Width(rect.width().max(1.0) as u32),
        );
        match texture {
            Ok(egui::load::TexturePoll::Ready { texture }) => {
                let img = texture.size;
                let img_aspect = if img.y > 0.0 { img.x / img.y } else { 1.0 };
                let rect_aspect = rect.height().max(1.0).recip() * rect.width();
                let uv = if rect_aspect > img_aspect {
                    let h = (img_aspect / rect_aspect).clamp(0.0, 1.0);
                    egui::Rect::from_min_max(
                        egui::pos2(0.0, (1.0 - h) / 2.0),
                        egui::pos2(1.0, (1.0 + h) / 2.0),
                    )
                } else {
                    let w = (rect_aspect / img_aspect).clamp(0.0, 1.0);
                    egui::Rect::from_min_max(
                        egui::pos2((1.0 - w) / 2.0, 0.0),
                        egui::pos2((1.0 + w) / 2.0, 1.0),
                    )
                };
                ui.put(
                    rect,
                    egui::Image::from_texture(texture)
                        .uv(uv)
                        .fit_to_exact_size(rect.size())
                        .maintain_aspect_ratio(false)
                        .corner_radius(corner),
                );
                return false;
            }
            Ok(egui::load::TexturePoll::Pending { .. }) => {
                // No spinner — the neutral tile stays until the art loads in the background.
                return false;
            }
            Err(_) if !is_last => {}
            Err(_) => return true,
        }
    }
    true
}

/// The Steam artwork URLs to try for an app, in order of preference. Steam has no single image that
/// exists for every app — some (unreleased titles, soundtracks, some DLC) are missing the classic
/// `header.jpg` but do have a capsule or a portrait library image — so the caller falls through the
/// list until one loads. Every entry is a real, per-app CDN path when it exists.
fn steam_artwork_urls(app_id: u32) -> [String; 5] {
    let base = "https://cdn.cloudflare.steamstatic.com/steam/apps";
    [
        format!("{base}/{app_id}/header.jpg"),
        // Newer / unreleased titles often have only the wide hero art on the CDN (header.jpg and the
        // capsules 404), so try it before the capsules — otherwise the library shows a blank tile.
        format!("{base}/{app_id}/library_hero.jpg"),
        format!("{base}/{app_id}/capsule_616x353.jpg"),
        format!("{base}/{app_id}/capsule_231x87.jpg"),
        format!("{base}/{app_id}/library_600x900.jpg"),
    ]
}

/// Loads the active-Denuvo App IDs: a fresh cache when available, otherwise a live curator fetch
/// (persisted for next time), falling back to any stale cache if the network is unavailable.
fn fetch_denuvo_appids(cache_path: &Path, force: bool) -> Result<Vec<u32>, String> {
    const DENUVO_CACHE_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
    if !force && let Some(appids) = load_cached_denuvo_appids(cache_path, DENUVO_CACHE_LIFETIME) {
        return Ok(appids);
    }
    match DenuvoWatchClient::new().and_then(|client| client.fetch_active_appids()) {
        Ok(appids) => {
            let _ = save_denuvo_appids(cache_path, &appids);
            Ok(appids)
        }
        Err(error) => read_denuvo_appids(cache_path).ok_or_else(|| error.to_string()),
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn path_if_present(value: &str) -> Option<&Path> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| Path::new(trimmed))
}

fn refresh_marker_is_current(path: &Path, expected: &str, maximum_age: Duration) -> bool {
    fs::read_to_string(path).is_ok_and(|value| value == expected)
        && fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age <= maximum_age)
}

fn github_access_mode() -> &'static str {
    if std::env::var("DRYDOCK_GITHUB_TOKEN").is_ok_and(|token| !token.trim().is_empty()) {
        "authenticated"
    } else {
        "anonymous"
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    #[test]
    fn response_code_paste_is_normalized_and_distributed() {
        let mut characters = std::array::from_fn(|_| String::new());
        let focus = distribute_response_code(&mut characters, 0, " ab-12 cd34 ");
        assert_eq!(characters.concat(), "AB12CD34");
        assert_eq!(focus, 7);
    }

    #[test]
    fn response_code_paste_respects_start_and_length() {
        let mut characters = std::array::from_fn(|_| String::new());
        let focus = distribute_response_code(&mut characters, 6, "xyTooLong");
        assert_eq!(characters[6], "X");
        assert_eq!(characters[7], "Y");
        assert_eq!(focus, 7);
    }

    #[test]
    fn only_eight_alphanumeric_characters_are_a_short_request() {
        assert!(is_short_activation_code("AB12CD34"));
        assert!(!is_short_activation_code("AB12-CD34"));
        assert!(!is_short_activation_code("CSL1.long.request"));
    }

    #[test]
    fn catalog_refresh_marker_prevents_rapid_restart_requests() {
        let directory = tempfile::tempdir().expect("tempdir");
        let marker = directory.path().join("catalog-refresh.marker");
        assert!(!refresh_marker_is_current(
            &marker,
            "0.1.0:anonymous",
            CATALOG_REFRESH_COOLDOWN
        ));
        fs::write(&marker, "0.1.0:anonymous").expect("marker");
        assert!(refresh_marker_is_current(
            &marker,
            "0.1.0:anonymous",
            CATALOG_REFRESH_COOLDOWN
        ));
        assert!(!refresh_marker_is_current(
            &marker,
            "0.1.0:authenticated",
            CATALOG_REFRESH_COOLDOWN
        ));
    }
}
