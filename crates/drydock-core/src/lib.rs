pub mod activation;
pub mod administrator;
pub mod app_payloads;
pub mod catalog;
pub mod cloud;
pub mod config;
pub mod conflicts;
pub mod denuvo;
pub mod depot;
pub mod download_queue;
pub mod emu_load_dlls;
pub mod emu_template;
pub mod emu_toolchain;
pub mod fixes;
pub mod game_folder;
pub mod harden;
pub mod language;
pub mod manifest_protection;
pub mod mfb;
pub mod models;
pub mod open_steam_tool;
pub mod paths;
pub mod proxy;
pub mod repacks;
pub mod safe_path;
pub mod self_test;
pub mod settings;
pub mod steam;
pub mod steam_appinfo;
pub mod steam_process;
pub mod steam_service;
pub mod steam_uri;
pub mod store;
pub mod ubisoft;
pub mod updater;
pub mod version;

pub use activation::{ActivationError, ActivationRequestService, VerifiedEntitlement, normalize_short_code};
pub use administrator::{ElevationError, ensure_elevated};
pub use app_payloads::{AppPayloadStore, StoredPayload};
pub use catalog::{
    AppCatalog, CatalogApp, CatalogError, RemoteCatalogClient, load_catalog_apps, save_catalog_apps,
};
pub use cloud::{
    CloudError, CloudProvider, CloudRedirect, CloudSettings, DllStatus, DownloadedDll, S3Credentials,
};
pub use config::{
    ConfigEntry, Source as ConfigSource, UserOverrides, describe as describe_config, set_user_overrides,
};
pub use conflicts::{ConflictingSoftwareStatus, detect_conflicting_software};
pub use denuvo::{
    DenuvoError, DenuvoWatchClient, load_cached_denuvo_appids, read_denuvo_appids, save_denuvo_appids,
};
pub use depot::{
    CdnClient, DepotData, DepotDownloadError, DepotManifest, DownloadOutcome, DownloadProgress,
    DownloadStage, VerifyOutcome,
};
pub use download_queue::QueueEffect;
pub use emu_template::{
    EmuTemplateError, EmuTemplateInput, PeArch, achievement_image_urls, detect_pe_arch, zip_files,
};
pub use emu_toolchain::{
    EmuToolchainError, ToolchainDll, ensure_toolchain, fetch_achievement_images, fetch_reframework_dll,
    load_dll_files, overlay_sound_bytes, toolchain_dlls, toolchain_ready,
};
pub use fixes::{FixError, FixStatus, apply_denuvo_fix, fix_status};
pub use game_folder::{CRACK_ARTIFACT_NAMES, remove_paths, resolve_game_root, scan_crack_files};
pub use harden::harden_dll_search;
pub use language::{GameLanguageError, GameLanguageOptions, apply_language, read_language_options};
pub use manifest_protection::{ManifestProtectionError, set_manifest_updates_enabled, updates_enabled};
pub use mfb::{
    DenuvoFix, FixEntry, RepositoryFile, SteamServiceManifest, SteamServicePackage, compute_git_blob_sha,
    matches_git_blob_sha,
};
pub use models::{AddedAppState, AppInfo, SteamManifest};
pub use open_steam_tool::{OpenSteamTool, OstError};
pub use paths::PortablePaths;
pub use proxy::{ProxyClient, ProxyError, hmac_secret, proxy_base_url};
pub use repacks::{LinkError, RepackApp, RepackSource, is_http_url, open_link};
pub use safe_path::{is_safe_path_segment, join_within, safe_segments};
pub use self_test::{SelfTestError, SelfTestReport, run_self_test};
pub use settings::{InstalledGame, LoadOutcome, QueuedDownload, Settings, SettingsError};
pub use steam::{SteamDiscovery, SteamError, discover_steam, is_valid_steam_directory, load_manifests};
pub use steam_appinfo::{
    SteamAppInfoError, fetch_install_dir, fetch_windows_arch, fetch_windows_executables,
};
pub use steam_process::{SteamProcessError, is_steam_running, restart_steam, start_steam, stop_steam};
pub use steam_service::{
    ServiceError, SteamServiceState, SteamServiceStatus, add_app_files, build_app_payload, has_app_lua_files,
    install_depot_manifests, install_service, installed_app_luas, remove_app_files, service_status,
    uninstall_service,
};
pub use steam_uri::{SteamUriAction, SteamUriError, open_steam_uri};
pub use store::{
    SteamReviewSummary, SteamStoreClient, SteamStoreDetails, SteamStoreError, SteamSystemRequirements,
    StoreCapsule, StoreFeatured,
};
pub use ubisoft::{
    TOKEN_FILE, TOKEN_REQUEST_FILE, UbisoftError, clear_previous_token_files, install_magicfiles,
    run_and_capture_token_request, token_directory,
};
pub use updater::{AppUpdater, AppVersion, PreparedUpdate, UpdateError};
pub use version::APP_VERSION;
