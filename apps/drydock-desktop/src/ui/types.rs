use egui::Color32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use std::sync::mpsc::Receiver;

use drydock_core::*;
use crate::ui::theme::*;

pub use crate::ui::components::header_resolver::HeaderResolver;

/// Cached result of validating the Steam directory draft text field.
#[derive(Clone, Debug, PartialEq)]
pub enum SteamDirValidation {
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
pub struct RateLimiter {
    pub window: Duration,
    pub max: usize,
    pub hits: Vec<Instant>,
}

impl RateLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            window,
            max,
            hits: Vec::new(),
        }
    }

    /// Records a request. Returns `Ok` if within the limit, or `Err(retry_after)` if not.
    pub fn check(&mut self) -> Result<(), Duration> {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
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
pub enum StoreTab {
    #[default]
    Featured,
    NewReleases,
    Repacks,
    DenuvoWatch,
}

impl StoreTab {
    pub const ALL: [(Self, &'static str); 4] = [
        (Self::Featured, "Featured"),
        (Self::NewReleases, "New Releases"),
        (Self::Repacks, "Repacks"),
        (Self::DenuvoWatch, "Denuvo"),
    ];
}

/// Which walkthrough the How It Works page is showing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuideFlow {
    Activation,
    Fixes,
}

/// Which store's activation the Activation page is currently showing (switched in-place, not a
/// separate page). Steam is the full flow; Ubisoft is the magicfiles/token.ini flow; EA is planned.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ActivationProvider {
    #[default]
    Steam,
    Ubisoft,
    Ea,
}

/// Home-search filter that restricts results to apps with a repack, optionally from one repacker.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RepackFilter {
    #[default]
    Any,
    /// Any app that has at least one repack source.
    AnyRepack,
    /// Only apps with a repack from this exact repacker (e.g. "DODI", "FitGirl").
    Repacker(String),
}

impl RepackFilter {
    /// Short label for the filter dropdown's button.
    pub fn label(&self) -> String {
        match self {
            Self::Any => "Any".to_owned(),
            Self::AnyRepack => "All repacks".to_owned(),
            Self::Repacker(name) => name.clone(),
        }
    }
}

/// Home-search filter that restricts results to apps with a fix of the given kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FixFilter {
    #[default]
    Any,
    Denuvo,
}

impl FixFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any",
            Self::Denuvo => "Denuvo",
        }
    }
}


pub struct DrydockApp {
    pub page: Page,
    /// The page shown on the previous frame, used to run a state refresh on every page switch.
    pub last_page: Page,
    /// When the Steam Service status was last re-checked from a page switch (throttled).
    pub last_service_check: Option<Instant>,
    pub guide_flow: GuideFlow,
    pub paths: PortablePaths,
    pub settings: Settings,
    /// Set when the settings file on disk could not be parsed or read at startup. `settings` then
    /// holds defaults, so every save is blocked until the user explicitly discards the broken file —
    /// otherwise the first write would replace their real library with an empty one.
    pub settings_read_only: bool,
    pub steam: SteamDiscovery,
    pub conflicts: ConflictingSoftwareStatus,
    pub manifests: Vec<SteamManifest>,
    pub catalog: Vec<CatalogApp>,
    /// App IDs present in the catalog (proxy gamelist), for O(1) "is this game available?" checks —
    /// used to filter the storefront shelves down to games Drydock can actually get.
    pub catalog_ids: std::collections::HashSet<u32>,
    /// On-demand resolver for catalog rows' real header art (the App-ID CDN guesses 404 for
    /// hashed-CDN titles). Shared by the Denuvo tab and the nav-search results.
    pub header_resolver: HeaderResolver,
    pub catalog_receiver: Option<Receiver<Result<Vec<CatalogApp>, String>>>,
    pub available_tags: Vec<String>,
    pub selected_tag: Option<String>,
    // Games that actively use Denuvo, from the "Denuvo Watch" Steam curator — the complete,
    // always-current activation-needed set, fetched once and cached, so the filter is instant.
    pub denuvo_appids: std::collections::HashSet<u32>,
    pub denuvo_loaded: bool,
    pub denuvo_receiver: Option<Receiver<Result<Vec<u32>, String>>>,
    // Per-app game fixes available in the GitHub `Files/fix` folder (Lua + zip).
    pub fixes: Vec<FixEntry>,
    pub fixes_loaded: bool,
    pub fixes_receiver: Option<Receiver<Result<Vec<FixEntry>, String>>>,
    // Per-app repacks (external download links) served by the proxy from `Files/repacks.json`.
    pub repacks: Vec<RepackApp>,
    pub repacks_loaded: bool,
    pub repacks_receiver: Option<Receiver<Result<Vec<RepackApp>, String>>>,
    // Home-search filters, plus the lookup structures they need (rebuilt when repacks/fixes load so
    // filtering the full catalog stays O(1) per app instead of scanning the repack/fix lists).
    pub repack_filter: RepackFilter,
    pub fix_filter: FixFilter,
    /// Distinct repacker names across all repacks, sorted, for the Repacks filter dropdown.
    pub available_repackers: Vec<String>,
    /// app_id -> lowercased repacker names for that app, for repack-filter matching.
    pub repackers_by_app: std::collections::HashMap<u32, Vec<String>>,
    /// The set of apps that have a Denuvo fix available, for fix-filter matching.
    pub fix_flags_by_app: std::collections::HashSet<u32>,
    // App whose Steam updates should be blocked once the running Apply Fix succeeds.
    pub pending_fix_block: Option<u32>,
    pub catalog_limiter: RateLimiter,
    pub download_limiter: RateLimiter,
    pub search: String,
    pub steam_directory_draft: String,
    /// Cache of the last validated draft path and its result, to avoid re-checking every frame.
    pub validated_steam_path: Option<(String, SteamDirValidation)>,
    pub selected_app: Option<u32>,
    /// The game highlighted in the Steam-style Library rail (right-hand overview shows this one).
    pub library_selected: Option<u32>,
    /// The folder chosen for the "Add game to Drydock" flow; `Some` puts the Library into add-mode
    /// (pick which game the folder is), cleared when the game is added or the flow is cancelled.
    pub add_game_folder: Option<PathBuf>,
    /// The game-picker query for the "Add game to Drydock" flow.
    pub add_game_search: String,
    /// Background folder/exe detection for the "Add game to Drydock" flow.
    pub add_game_receiver: Option<Receiver<Result<AddGameOutcome, String>>>,
    /// Background detection that registers a just-finished depot download in the Drydock library.
    pub download_install_receiver: Option<Receiver<Result<AddGameOutcome, String>>>,
    pub language_options: Option<GameLanguageOptions>,
    pub language_directory: Option<PathBuf>,
    pub language_selection: String,
    /// The folder chosen in the Tools tab's language changer.
    pub tools_language_path: String,
    /// The App ID typed into the Tools emulator cracker.
    pub emu_appid: String,
    /// Deploy the loader proxy as `winmm.dll` instead of `version.dll`.
    pub emu_loader_winmm: bool,
    /// The emulator-cracker architecture choice (auto-detect, or forced x64/x86).
    pub emu_arch: EmuArch,
    /// Also bundle praydog's latest REFramework nightly (`dinput8.dll`) next to the exe (opt-in).
    pub emu_reframework: bool,
    /// Background job for the local emulator cracker (App ID → depots → files).
    pub emu_receiver: Option<Receiver<Result<String, String>>>,
    pub status: String,
    pub status_error: bool,
    pub background_action: Option<Receiver<Result<String, String>>>,
    pub busy_label: Option<String>,
    pub details_app_id: Option<u32>,
    pub store_details: Option<SteamStoreDetails>,
    pub store_receiver: Option<Receiver<(u32, Result<SteamStoreDetails, String>)>>,
    pub store_loading: bool,
    // Store tab (Steam-style storefront): the selected sub-tab and the live featured feed.
    pub store_tab: StoreTab,
    pub featured: Option<StoreFeatured>,
    pub featured_loading: bool,
    pub featured_error: Option<String>,
    pub featured_receiver: Option<Receiver<Result<StoreFeatured, String>>>,
    pub screenshot_index: usize,
    pub activation_request_code: String,
    pub activation_receiver: Option<Receiver<Result<String, String>>>,
    pub activation_verify_receiver: Option<Receiver<Result<VerifiedEntitlement, String>>>,
    pub verified_entitlement: Option<VerifiedEntitlement>,
    pub entitlement_success_app: Option<String>,
    pub response_code: [String; 8],
    // Foreign-install activation: dynamic game search, chosen/verified game folder, and the
    // background checks that resolve the install root and screen for crack/HV artifacts.
    pub activation_search: String,
    pub activation_path: String,
    pub activation_root: Option<PathBuf>,
    pub activation_check_app: Option<u32>,
    pub activation_check_receiver: Option<Receiver<Result<ActivationCheck, String>>>,
    pub activation_remove_receiver: Option<Receiver<Result<(u32, PathBuf), String>>>,
    pub pending_crack: Option<PendingCrack>,
    // Which store's activation is on screen, and the Ubisoft flow's state: the background
    // magicfiles+launch+capture step, the resulting activation code, and where token.ini installs.
    pub activation_provider: ActivationProvider,
    pub ubisoft_prepare_receiver: Option<Receiver<Result<UbisoftPrepared, String>>>,
    pub ubisoft_activation_code: String,
    pub ubisoft_exe_dir: Option<PathBuf>,
    pub update_receiver: Option<Receiver<Result<Option<PreparedUpdate>, String>>>,
    pub exit_for_update: bool,
    // Cloud tab (CloudRedirect): provider form + the background DLL download / OAuth sign-in.
    pub cloud: CloudForm,
    pub cloud_download_receiver: Option<Receiver<Result<DownloadedDll, String>>>,
    pub cloud_oauth_receiver: Option<Receiver<Result<CloudSettings, String>>>,
    /// Cached CloudRedirect DLL state — `dll_status` hashes the file, so it is refreshed on page
    /// switches and after cloud actions rather than read from the render loop. `None` = no Steam root.
    /// App IDs with an unlock Lua actually present in `<steam>/config/stplug-in`.
    ///
    /// That folder is the ground truth for what is unlocked; `settings.added_apps` is only a record
    /// of what *this* installation added, and it is lost whenever the settings file is. Refreshed on
    /// page switches and after every add/remove, so the render loop never touches the disk.
    pub plugin_luas: std::collections::BTreeSet<u32>,
    /// Drydock's own copy of each added app's Lua + depot manifests, so an install can restore them
    /// after Steam has deleted its `depotcache` entries on uninstall.
    pub payload_store: AppPayloadStore,
    pub cloud_dll_status: Option<drydock_core::DllStatus>,
    /// Cached provider from the CloudRedirect `config.json`, refreshed alongside the DLL state.
    pub cloud_provider: Option<CloudProvider>,
    pub service_status: Option<SteamServiceStatus>,
    pub service_receiver: Option<Receiver<Result<ServiceOutcome, String>>>,
    // Native depot download: the active download/verify job (background thread → progress channel),
    // shown in the bottom Downloads bar. The pending queue lives in `settings.download_queue` (front
    // = current), so it persists across launches and auto-resumes.
    pub download_job: Option<DownloadJob>,
    /// The current download is paused (by the user, or stopped by an error) — its queue entry stays
    /// at the front so it can resume, but no thread is running.
    pub download_paused: bool,
    /// When a download stopped because of an error (not a user pause), the message to show.
    pub download_error: Option<String>,
    /// The most recent progress tick, kept so a paused download still shows its position in the bar.
    pub download_last: Option<DownloadProgress>,
    /// Set when the running thread is being cancelled only to immediately start a reordered front
    /// (activate a queued download, or send the active one back into the queue) — not a pause.
    pub download_switch_pending: bool,
    pub started: Instant,
}


/// A running (or just-finished) depot download or verify, driven by a background thread.
pub struct DownloadJob {
    pub app_id: u32,
    pub name: String,
    pub kind: DownloadKind,
    pub cancel: Arc<AtomicBool>,
    pub receiver: Receiver<DownloadUpdate>,
    pub progress: Option<DownloadProgress>,
    /// Smoothed download speed in bytes/sec, its running peak, plus the last (time, done_bytes)
    /// sample the estimate came from.
    pub speed_bps: f64,
    pub peak_bps: f64,
    pub sample: Option<(Instant, u64)>,
    /// `Some` once the job ended: `Ok(summary)` or `Err(message)`.
    pub finished: Option<Result<String, String>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DownloadKind {
    Download,
    Verify,
}

pub enum DownloadUpdate {
    Progress(DownloadProgress),
    Finished(Result<String, String>),
}

/// Outcome of the pre-activation folder check: the verified install root, and whether crack/HV
/// artifacts must be removed before a request code is generated.
pub enum ActivationCheck {
    Ready { root: PathBuf },
    NeedsRemoval { root: PathBuf, files: Vec<PathBuf> },
}

/// Result of the Ubisoft "prepare" step: the activation code to paste into a ticket, and the exe
/// directory the response token's `token.ini` will be installed into.
pub struct UbisoftPrepared {
    pub activation_code: String,
    pub exe_dir: PathBuf,
}

/// State for the "crack files found" prompt: the app and verified root, and the paths to delete.
pub struct PendingCrack {
    pub app_id: u32,
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

/// Cloud-tab form state: the selected CloudRedirect provider and its path/credential fields.
pub struct CloudForm {
    pub provider: CloudProvider,
    pub folder_path: String,
    pub local_path: String,
    pub account_id: String,
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub key_prefix: String,
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
    pub fn to_settings(&self) -> Option<CloudSettings> {
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
pub enum ServiceOutcome {
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

pub enum StoreAction {
    Details(u32),
    Activate(u32),
}


pub struct AddGameOutcome {
    pub app_id: u32,
    pub name: String,
    pub root: PathBuf,
    pub exe: PathBuf,
}

/// Where a Library game comes from — decides its rail group and which management buttons it gets.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibrarySource {
    /// Steam has it installed (a Steam `appmanifest.acf`).
    SteamInstalled,
    /// Drydock downloaded it through its own depot engine.
    DrydockInstalled,
    /// Its unlock Lua is in Steam, but the game itself isn't installed yet.
    Available,
}

/// One row of the Library page: a game the user owns, with enough state to render its Play button.
pub struct LibraryEntry {
    pub app_id: u32,
    pub name: String,
    /// Steam has this game installed (it launches through `steam://`).
    pub installed: bool,
    /// A remembered `.exe` for a game activated outside Steam, launched directly.
    pub launch_path: Option<String>,
    /// Which group/buttons this game belongs to.
    pub source: LibrarySource,
}

/// What a Library card wants the page to do once the frame is laid out (applied after the borrow of
/// the entries list ends).
pub enum LibraryAction {
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


pub enum EmuOutput {
    /// Deploy the files into this game install folder (root).
    Deploy(PathBuf),
    /// Save the files as a ZIP (with the game's exe-subfolder path structure) at this path.
    Zip(PathBuf),
}

/// The architecture choice for the emulator cracker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EmuArch {
    /// Detect from Steam app-info (osarch / exe path), then the game exe's PE header for a deploy.
    Auto,
    X64,
    X86,
}

/// Guesses the architecture from a depot's file layout: a Windows exe/dll under a `win64`/`bin64`/
/// `x64` path means x64, `win32`/`x86` means x86. Used as an account-free fallback when Steam

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum SteamServiceCardAction {
    None,
    Install,
    Uninstall,
    Reinstall,
    Restart,
}

/// A compact bottom-left status card: a coloured accent dot with a title and optional detail.
pub struct Notification {
    pub accent: Color32,
    pub title: String,
    #[allow(dead_code)]
    pub detail: String,
}

impl Notification {
    pub fn error(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: DANGER,
            title: title.into(),
            detail: detail.into(),
        }
    }

    pub fn warn(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: AMBER,
            title: title.into(),
            detail: detail.into(),
        }
    }

    pub fn ok(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: ACCENT_SOFT,
            title: title.into(),
            detail: detail.into(),
        }
    }

    pub fn info(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            accent: ACCENT,
            title: title.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct DepotButtons {
    /// The proxy has key + manifests for this app.
    pub downloadable: bool,
    /// The game is installed (so Verify makes sense).
    pub installed: bool,
    /// A download/verify job is currently running (buttons disabled).
    pub busy: bool,
}


#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DetailsAction {
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
pub struct FixPanelState {
    /// A background action (e.g. this fix, or an add-to-Steam) is running.
    pub busy: bool,
    /// The game is installed, so there is a folder to extract the fix into.
    pub installed: bool,
    /// The Denuvo fix's installed status, if the app has a Denuvo fix.
    pub denuvo: Option<FixStatus>,
}

/// The repacker names for the app on the details page, one Download button each. The link for each
/// is resolved by index from the app's repack sources when the button is clicked.
#[derive(Clone)]
pub struct RepackPanelState {
    pub repackers: Vec<String>,
}

/// The optional action panels shown on the details page: the Apply-Fix buttons and the repack
/// Download buttons. Bundled so the render helpers stay under the argument limit.
#[derive(Clone, Copy)]
pub struct DetailsPanels<'a> {
    pub fix: Option<&'a FixPanelState>,
    pub repack: Option<&'a RepackPanelState>,
}

/// The state that decides which app-page buttons are shown and enabled.
#[derive(Clone, Copy)]
pub struct DetailsState {
    /// The unlock Lua has been added to the Steam plug-in folder.
    pub is_added: bool,
    /// The Steam Service is current, so an app may be added.
    pub service_current: bool,
    /// A Steam Service background operation is in progress.
    pub busy: bool,
}

