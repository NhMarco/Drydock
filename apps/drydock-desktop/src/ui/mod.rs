use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

use drydock_core::*;
use eframe::egui::{self, Align, Layout, Vec2};

pub mod components;
pub mod helpers;
pub mod pages;
pub mod theme;
pub mod types;
pub mod widgets;

pub use components::*;
pub use helpers::*;
pub use theme::*;
pub use types::*;

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
        context
            .egui_ctx
            .options_mut(|options| options.reduce_texture_memory = true);
        let image_textures = Arc::new(crate::image_cache::VisibleTextureLoader::default());
        context.egui_ctx.add_texture_loader(image_textures.clone());
        // Recovering load: a corrupt/half-written settings file falls back to the `.bak` copy, and a
        // file that cannot be salvaged is moved aside rather than silently replaced by defaults —
        // otherwise the next save would wipe `added_apps` / `installed_games` / `launch_paths`.
        let (settings, load_outcome) = Settings::load_recovering(&paths.settings_file());
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
        let steam_directory_draft = steam.root.as_ref().map_or_else(
            || settings.steam_directory.clone(),
            |path| path.display().to_string(),
        );
        let games_directory_draft = settings.games_directory.clone();
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

        // Built before the struct literal, which moves `paths`.
        let payload_store = AppPayloadStore::new(&paths.settings_dir());
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
            games_directory_draft,
            last_unlock_update_check: None,
            launch_warned: std::collections::HashSet::new(),
            manifest_guard_receiver: None,
            manifest_fetch_attempted: std::collections::HashSet::new(),
            last_manifest_check: None,
            steam_was_running: drydock_core::is_steam_running(),
            activation_verify: None,
            image_textures,
            unlock_writes: Arc::default(),
            unlock_update_receiver: None,
            validated_steam_path: None,
            selected_app: None,
            library_selected: None,
            library_modal: None,
            library_search: String::new(),
            library_filter: LibraryFilter::default(),
            add_game_folder: None,
            add_game_search: String::new(),
            add_game_receiver: None,
            download_install_receiver: Vec::new(),
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
            hero_carousel_index: 0,
            hero_last_scroll: None,
            see_all_section: None,
            notifications_open: false,
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
            plugin_luas: std::collections::BTreeSet::new(),
            payload_store,
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
        app.release_old_update_blocks();
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
    pub fn recompute_tags(&mut self) {
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

    pub fn start_catalog_refresh(&mut self, force: bool) {
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
    pub fn force_catalog_refresh(&mut self) {
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

    pub fn poll_catalog_refresh(&mut self) {
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
    pub fn needs_activation(&self, app_id: u32) -> Option<bool> {
        self.denuvo_loaded.then(|| self.denuvo_appids.contains(&app_id))
    }

    /// Fetches the "Denuvo Watch" curator list in the background (cached for a day). Falls back to
    /// any cached list if the network fetch fails, so the filter still works offline.
    pub fn start_denuvo_refresh(&mut self, force: bool) {
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

    pub fn poll_denuvo_refresh(&mut self) {
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
    pub fn start_fixes_refresh(&mut self) {
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

    pub fn poll_fixes_refresh(&mut self) {
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

    pub fn start_repacks_refresh(&mut self) {
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

    pub fn poll_repacks_refresh(&mut self) {
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
    pub fn repack_for(&self, app_id: u32) -> Option<&RepackApp> {
        self.repacks.iter().find(|repack| repack.app_id == app_id)
    }

    /// The available fix for an app, if any.
    pub fn fix_for(&self, app_id: u32) -> Option<&FixEntry> {
        self.fixes.iter().find(|fix| fix.app_id == app_id)
    }

    /// Rebuilds the repack lookup (app -> repacker names) and the distinct-repacker list the Home
    /// filter dropdown offers. Called whenever the repack list is (re)loaded.
    pub fn rebuild_repack_index(&mut self) {
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
    pub fn rebuild_fix_index(&mut self) {
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
    pub fn apply_denuvo_fix_for(&mut self, app_id: u32) {
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
    pub fn start_activation_check(&mut self, app_id: u32, chosen: PathBuf) {
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

    pub fn poll_activation_check(&mut self) {
        let Some(receiver) = self.activation_check_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_check_receiver = None;
                self.busy_label = None;
                let app_id = self.activation_check_app.take();
                match result {
                    Ok(ActivationCheck::Ready { root }) => match app_id {
                        Some(app_id) => self.continue_activation(app_id, root),
                        None => {
                            self.activation_path = root.display().to_string();
                            self.activation_root = Some(root);
                        }
                    },
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
    pub fn start_crack_removal(&mut self) {
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

    pub fn poll_activation_remove(&mut self) {
        let Some(receiver) = self.activation_remove_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_remove_receiver = None;
                self.busy_label = None;
                match result {
                    Ok((app_id, root)) => {
                        self.status = "Crack files removed.".into();
                        self.status_error = false;
                        self.continue_activation(app_id, root);
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

    /// With a clean game folder, verifies the game's files first when the user asked for that, and
    /// otherwise makes the request code straight away. The verify runs before anything is activated:
    /// an activation installs its own files, which a verify afterwards would report as damaged.
    pub fn continue_activation(&mut self, app_id: u32, root: PathBuf) {
        self.activation_path = root.display().to_string();
        self.activation_root = Some(root.clone());
        if !self.settings.verify_before_activation {
            self.generate_activation_request(app_id);
            return;
        }
        if self.download_running() {
            self.status = "A download is running, so the game can't be verified now. Wait for it to finish, \
                           or turn off the verification."
                .into();
            self.status_error = true;
            return;
        }
        let name = self.app_display_name(app_id);
        self.spawn_job(app_id, name, DownloadKind::Verify, Some(root.clone()));
        self.activation_verify = Some((app_id, root));
        self.status = "Verifying the game files before activation…".into();
        self.status_error = false;
    }

    /// Continues an activation that waited for its verify: the request code is made only when every
    /// file checked out. Returns whether the finished verify belonged to one.
    pub fn finish_activation_verify(
        &mut self,
        app_id: u32,
        verified: Option<bool>,
        result: &Result<String, String>,
    ) -> bool {
        if self
            .activation_verify
            .as_ref()
            .is_none_or(|(pending, _)| *pending != app_id)
        {
            return false;
        }
        self.activation_verify = None;
        match (result, verified) {
            (Ok(_), Some(true)) => self.generate_activation_request(app_id),
            (Ok(summary), _) => {
                self.status =
                    format!("{summary}. Repair the game before activating it, or turn off the verification.");
                self.status_error = true;
            }
            (Err(error), _) => {
                self.status = format!(
                    "The game could not be verified: {error}. Try again, or turn off the verification."
                );
                self.status_error = true;
            }
        }
        true
    }

    pub fn generate_activation_request(&mut self, app_id: u32) {
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

    pub fn verify_activation_response(&mut self, app_id: u32) {
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

    pub fn poll_activation_verification(&mut self) {
        let Some(receiver) = self.activation_verify_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_verify_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(entitlement) => {
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
                            "Activation applied for App {} — game files installed.",
                            entitlement.app_id
                        );
                        self.status_error = false;
                        self.verified_entitlement = Some(entitlement);
                    }
                    Err(error) => {
                        self.status = error;
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

    pub fn start_featured(&mut self) {
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

    pub fn poll_featured(&mut self) {
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

    pub fn restart_steam_in_background(&mut self) {
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

    pub fn poll_background_action(&mut self) {
        let Some(receiver) = self.background_action.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.background_action = None;
                self.busy_label = None;
                self.pending_fix_block = None;
                match result {
                    Ok(status) => {
                        self.status = status;
                        self.status_error = false;
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
    pub fn refresh_service_status(&mut self) {
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
    pub fn install_steam_service(&mut self) {
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
    pub fn uninstall_steam_service(&mut self) {
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
    pub fn add_app_to_steam(&mut self, app_id: u32) {
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
        // Naming the second half matters: fetching the depot package is the slow part (the upstream
        // builds it on demand), so without this the button looks stuck on a large title.
        self.busy_label = Some(format!("Adding {name} to Steam and caching its manifests…"));
        let store = self.payload_store.clone();
        let writes = Arc::clone(&self.unlock_writes);
        writes.touch(app_id);
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
                // One fetch for both halves. The package's own Lua is the one to install: it is cut
                // from the same build as the manifests beside it, so its `setManifestid` lines are
                // active and pin exactly those GIDs. The Lua API answers for the *current* build
                // instead, with the pins commented out — Steam then resolves a build of its own
                // choosing and the manifests cached here are never the ones it asks for.
                let depot = DepotData::fetch(&client, app_id);
                let (file_name, bytes, manifests) = match &depot {
                    Ok(data) if data.lua.is_some() => {
                        let (name, bytes) = data.lua.clone().expect("checked above");
                        (name, bytes, Ok(&data.raw_manifests))
                    }
                    // No package, or one without a Lua: fall back to the Lua API so the app can
                    // still be unlocked, and say so — that unlock is not pinned to a build.
                    other => {
                        let bytes = client.download_lua(app_id).map_err(|error| error.to_string())?;
                        let error = match other {
                            Err(error) => error.to_string(),
                            Ok(_) => "the depot package carried no unlock Lua".to_owned(),
                        };
                        (format!("{app_id}.lua"), bytes, Err(error))
                    }
                };
                let installed_names = vec![file_name.clone()];
                let mut payload = std::collections::BTreeMap::new();
                payload.insert(file_name.clone(), bytes.clone());
                let _write = writes.write();
                add_app_files(&root, &payload).map_err(|error| error.to_string())?;

                let mut backup_note = String::new();
                let installed = match manifests {
                    Ok(raw) => {
                        let count = install_depot_manifests(&root, raw).map_err(|e| e.to_string());
                        // Our own copy of both, so a later reinstall needs no network. Failing to
                        // keep it does not undo an otherwise successful add, so it is reported beside
                        // the result rather than as it.
                        backup_note = payload_backup_note(&store, app_id, Some((&file_name, &bytes)), raw);
                        count
                    }
                    Err(error) => Err(error),
                };
                Ok(ServiceOutcome::Added {
                    app_id,
                    files: installed_names,
                    note: added_note(&name, &installed) + &backup_note,
                    source: UnlockSource::Latest,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Adds the GitHub build-locked "Denuvo fix" Lua to the Steam plug-in folder in place of the
    /// normal token Lua, pinning the game to the cracked build. Downloads and installs on a
    /// background thread; the game files themselves are applied separately via APPLY DENUVO FIX.
    pub fn add_cracked_to_steam(&mut self, app_id: u32) {
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
        };
        let name = self.app_display_name(app_id);
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some(format!(
            "Adding the cracked version of {name} to Steam and caching its manifests…"
        ));
        let store = self.payload_store.clone();
        let writes = Arc::clone(&self.unlock_writes);
        writes.touch(app_id);
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
                payload.insert(file_name.clone(), bytes.clone());
                let _write = writes.write();
                add_app_files(&root, &payload).map_err(|error| error.to_string())?;
                // Same as the normal add. A manifest file is named after the exact depot and
                // manifest it belongs to, so caching the provider's current build alongside a
                // build-locked Lua can never mislead Steam — it just will not find a name it is not
                // looking for.
                let manifests =
                    copy_depot_manifests_to_cache(&client, &root, &store, app_id, Some((&file_name, &bytes)));
                Ok(ServiceOutcome::Added {
                    app_id,
                    files: installed_names,
                    note: format!(
                        "Cracked version of \"{name}\" added to Steam{}. Apply the Denuvo fix, then restart Steam.{}",
                        match &manifests {
                            Ok((0, _)) | Err(_) => String::new(),
                            Ok((count, _)) => format!(" with {count} depot manifest(s)"),
                        },
                        manifests
                            .as_ref()
                            .map_or("", |(_, backup_note)| backup_note.as_str()),
                    ),
                    source: UnlockSource::Cracked,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Adds a Lua and/or depot manifests the user picks from disk. `app_id` is the game whose page this
    /// was started from; from the library, the game is taken from the Lua.
    pub fn add_own_unlock(&mut self, app_id: Option<u32>) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let Some(files) = crate::unlocks::pick_unlock_files() else {
            return;
        };
        let unlock = match read_own_unlock(&files, app_id) {
            Ok(unlock) => unlock,
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
                return;
            }
        };
        let app_id = unlock.app_id;
        let name = self.app_display_name(app_id);
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        self.busy_label = Some(format!("Adding your files for {name} to Steam…"));
        let store = self.payload_store.clone();
        let writes = Arc::clone(&self.unlock_writes);
        writes.touch(app_id);
        std::thread::spawn(move || {
            let result = (|| {
                let manifest = OpenSteamTool::new()
                    .and_then(|ost| ost.manifest())
                    .map_err(|error| error.to_string())?;
                if service_status(&root, &manifest).state != SteamServiceState::Current {
                    return Err("Install or update the Steam Service before adding an app.".to_owned());
                }
                let note = crate::unlocks::install_own_unlock(&root, &store, &unlock, &writes, &name)?;
                Ok(ServiceOutcome::Added {
                    app_id,
                    files: unlock.lua.iter().map(|_| unlock.lua_file_name()).collect(),
                    note,
                    source: UnlockSource::Own,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Opens the Activation tab with `app_id` preselected (from the details ACTIVATE button).
    pub fn remove_app_from_steam(&mut self, app_id: u32) {
        let Some(root) = self.steam_root_or_error("Steam was not found. Select its folder in Settings.")
        else {
            return;
        };
        if self.service_receiver.is_some() {
            return;
        }
        let name = self.app_display_name(app_id);
        // Names to remove come from what we recorded when the app was added — plus the conventional
        // `<appid>.lua`, because a Lua that this installation did not add (a fresh settings file, a
        // moved data directory, a hand-placed file) has no record at all and would otherwise leave
        // the remove doing nothing.
        let mut names: Vec<String> = self
            .settings
            .added_apps
            .get(&app_id)
            .map(|state| state.files.keys().cloned().collect())
            .unwrap_or_default();
        let conventional = format!("{app_id}.lua");
        if self.plugin_luas.contains(&app_id) && !names.iter().any(|n| n.eq_ignore_ascii_case(&conventional))
        {
            names.push(conventional);
        }
        let (sender, receiver) = mpsc::channel();
        self.service_receiver = Some(receiver);
        let writes = Arc::clone(&self.unlock_writes);
        writes.touch(app_id);
        std::thread::spawn(move || {
            let result = (|| {
                // Fall back to the deterministic Ryuu file name when nothing was recorded.
                if names.is_empty() {
                    names = vec![format!("{app_id}.lua")];
                }
                let _write = writes.write();
                remove_app_files(&root, &names).map_err(|error| error.to_string())?;
                let note = format!("\"{name}\" removed from Steam.");
                Ok(ServiceOutcome::Removed { app_id, note })
            })();
            let _ = sender.send(result);
        });
    }

    pub fn app_display_name(&self, app_id: u32) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.app_id == app_id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| format!("App {app_id}"))
    }

    /// Returns the Steam root path, or sets an error status and returns `None` if missing.
    pub fn steam_root_or_error(&mut self, message: &str) -> Option<PathBuf> {
        let root = self.steam.root.clone();
        if root.is_none() {
            self.status = message.to_owned();
            self.status_error = true;
        }
        root
    }

    pub fn poll_service_action(&mut self) {
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
                    Ok(ServiceOutcome::Added {
                        app_id,
                        files,
                        note,
                        source,
                    }) => {
                        let mut state = AddedAppState {
                            source: Some(source),
                            ..AddedAppState::default()
                        };
                        if files.is_empty()
                            && let Some(previous) = self.settings.added_apps.get(&app_id)
                        {
                            state.files.clone_from(&previous.files);
                        }
                        for file in files {
                            state.files.insert(file, String::new());
                        }
                        self.settings.added_apps.insert(app_id, state);
                        self.status_error = self.persist_settings().is_err();
                        self.status = note;
                        // The plug-in folder just changed; re-scan so the library and the details
                        // buttons reflect it immediately rather than at the next page switch.
                        self.refresh_plugin_luas();
                    }
                    Ok(ServiceOutcome::Removed { app_id, note }) => {
                        self.settings.added_apps.remove(&app_id);
                        // Removing an app from Steam is the user saying they are done with it, so
                        // the local copy goes too — unlike a Steam *uninstall*, which is exactly the
                        // case the store exists to survive.
                        let _ = self.payload_store.remove(app_id);
                        self.status_error = self.persist_settings().is_err();
                        self.status = note;
                        self.refresh_plugin_luas();
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
    pub fn refresh_plugin_luas(&mut self) {
        self.plugin_luas = self
            .steam
            .root
            .as_deref()
            .map(installed_app_luas)
            .unwrap_or_default();
    }

    /// Whether an app has an unlock Lua installed — on disk, or recorded by this installation.
    ///
    /// The disk is authoritative and comes first: a Lua sitting in `config/stplug-in` counts as
    /// added even when `settings.added_apps` knows nothing about it, which is the normal situation
    /// after the settings file is reset, the data directory moves, or the user drops a Lua in by
    /// hand. The settings record is still consulted so an app added moments ago is recognised before
    /// the next re-scan.
    pub fn is_app_added(&self, app_id: u32) -> bool {
        self.plugin_luas.contains(&app_id) || self.settings.added_apps.contains_key(&app_id)
    }

    pub fn refresh_dynamic_state(&mut self) {
        self.refresh_cloud_state();
        self.refresh_plugin_luas();
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
        self.guard_depot_manifests();
        let due = self
            .last_service_check
            .is_none_or(|at| at.elapsed() >= SERVICE_RECHECK_COOLDOWN);
        if due && self.service_receiver.is_none() {
            self.last_service_check = Some(Instant::now());
            self.refresh_service_status();
        }
    }

    pub fn release_old_update_blocks(&mut self) {
        let lua_apps = self
            .steam
            .root
            .as_deref()
            .map(installed_app_luas)
            .unwrap_or_default();
        let recorded_before = self.settings.legacy_update_blocks.len();
        let release = release_update_blocks(
            &self.manifests,
            &lua_apps,
            &mut self.settings.legacy_update_blocks,
        );
        if self.settings.legacy_update_blocks.len() != recorded_before {
            let _ = self.write_settings();
        }
        if let Some((app_id, error)) = release.failed.first() {
            self.status = format!(
                "The old Steam update block of {} game(s) could not be lifted (App {app_id}: {error}). \
                 Drydock tries again at the next start.",
                release.failed.len()
            );
            self.status_error = true;
        } else if !release.released.is_empty() && !self.status_error {
            self.status = format!(
                "Steam can update {} game(s) again: the update block older Drydock versions set was lifted.",
                release.released.len()
            );
        }
    }

    pub fn refresh_steam(&mut self) {
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
                self.release_old_update_blocks();
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
    /// Ordered pages the screenshot harness walks through (see [`crate::screenshot`]).
    #[cfg(feature = "screenshot")]
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
    #[cfg(feature = "screenshot")]
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
                    install_root: None,
                    verified: None,
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

    pub fn apps_following_latest(&self) -> Vec<u32> {
        let root = self.steam.root.as_deref();
        self.settings
            .added_apps
            .iter()
            .filter(|(app_id, state)| match state.source {
                Some(UnlockSource::Latest) => true,
                Some(UnlockSource::Cracked | UnlockSource::Own) => false,
                None => {
                    self.fixes_loaded
                        && !self
                            .fix_for(**app_id)
                            .and_then(|fix| fix.denuvo.as_ref())
                            .zip(root)
                            .is_some_and(|(denuvo, root)| fix_status(root, denuvo) == FixStatus::Applied)
                }
            })
            .map(|(app_id, _)| *app_id)
            .collect()
    }

    pub fn maybe_update_unlocks(&mut self) {
        if !self.settings.auto_update_unlocks || self.unlock_update_receiver.is_some() {
            return;
        }
        if self
            .last_unlock_update_check
            .is_some_and(|at| at.elapsed() < UNLOCK_UPDATE_POLL)
        {
            return;
        }
        self.last_unlock_update_check = Some(Instant::now());
        let Some(root) = self.steam.root.clone() else {
            return;
        };
        if self
            .service_status
            .as_ref()
            .is_none_or(|status| status.state != SteamServiceState::Current)
        {
            return;
        }
        let apps = self.apps_following_latest();
        let marker = self.paths.cache_dir().join("unlock-update.marker");
        if apps.is_empty() || refresh_marker_is_current(&marker, "v1", UNLOCK_UPDATE_INTERVAL) {
            return;
        }
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, "v1");
        let store = self.payload_store.clone();
        let writes = Arc::clone(&self.unlock_writes);
        let (sender, receiver) = mpsc::channel();
        self.unlock_update_receiver = Some(receiver);
        std::thread::spawn(move || {
            let sweep = match ProxyClient::new() {
                Ok(client) => crate::unlocks::update_unlocks(
                    &root,
                    &store,
                    &apps,
                    &writes,
                    UNLOCK_UPDATE_SPACING,
                    |app_id| match DepotData::fetch(&client, app_id) {
                        Ok(data) => Ok(data.lua.map(|(_, lua)| (lua, data.raw_manifests))),
                        Err(error) => Err(error.to_string()),
                    },
                ),
                Err(_) => crate::unlocks::UnlockUpdateSweep {
                    updated: Vec::new(),
                    failed: apps.len(),
                },
            };
            let _ = sender.send(sweep);
        });
    }

    pub fn poll_unlock_updates(&mut self) {
        let Some(receiver) = self.unlock_update_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(sweep) => {
                self.unlock_update_receiver = None;
                if sweep.updated.is_empty() {
                    return;
                }
                let mut recorded = false;
                for app_id in &sweep.updated {
                    if let Some(state) = self.settings.added_apps.get_mut(app_id)
                        && state.source.is_none()
                    {
                        state.source = Some(UnlockSource::Latest);
                        recorded = true;
                    }
                }
                if recorded {
                    let _ = self.persist_settings();
                }
                self.refresh_plugin_luas();
                let names: Vec<String> = sweep
                    .updated
                    .iter()
                    .take(3)
                    .map(|app_id| self.app_display_name(*app_id))
                    .collect();
                let more = sweep.updated.len().saturating_sub(names.len());
                self.status = format!(
                    "Updated the Lua and manifests of {}{} to the latest version.",
                    names.join(", "),
                    if more > 0 {
                        format!(" and {more} more")
                    } else {
                        String::new()
                    }
                );
                self.status_error = false;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.unlock_update_receiver = None,
        }
    }

    pub fn guard_depot_manifests(&mut self) {
        if self.manifest_guard_receiver.is_some() {
            return;
        }
        let Some(steam_root) = self.steam.root.clone() else {
            return;
        };
        let running = is_steam_running();
        let flipped = running != self.steam_was_running;
        self.steam_was_running = running;
        let due = self
            .last_manifest_check
            .is_none_or(|at| at.elapsed() >= MANIFEST_RECHECK_COOLDOWN);
        if !flipped && !due {
            return;
        }
        self.last_manifest_check = Some(Instant::now());
        // Each app with whether its manifests may come from the provider: files the user supplied
        // themselves are never mixed with the provider's.
        let apps: Vec<(u32, bool)> = self
            .settings
            .added_apps
            .iter()
            .map(|(app_id, state)| (*app_id, state.source != Some(UnlockSource::Own)))
            .collect();
        if apps.is_empty() {
            return;
        }
        let store = self.payload_store.clone();
        let fetch_attempted = self.manifest_fetch_attempted.clone();
        let (sender, receiver) = mpsc::channel();
        self.manifest_guard_receiver = Some(receiver);
        std::thread::spawn(move || {
            let mut restored = 0usize;
            let mut repaired = Vec::new();
            let mut attempted: Option<u32> = None;
            // Apps the add never managed to store manifests for. Caching the depot package is best
            // effort at add time — an upstream that is slow, down, or still packaging leaves the app
            // with its unlock and nothing else, and because nothing was stored the cheap local pass
            // below can never repair it. Those need a fetch, which is expensive, so they are only
            // collected here and one is retried at the end.
            let mut never_stored = Vec::new();
            for (app_id, from_provider) in apps {
                let index = store.manifest_index(app_id);
                if index.is_empty() {
                    if from_provider {
                        never_stored.push(app_id);
                    }
                    continue;
                }
                let missing = missing_depot_manifests(&steam_root, &index);
                if missing.is_empty() {
                    continue;
                }
                // Only now is it worth reading the manifests back into memory.
                let wanted: std::collections::BTreeMap<String, Vec<u8>> = store
                    .load(app_id)
                    .manifests
                    .into_iter()
                    .filter(|(name, _)| missing.contains(name))
                    .collect();
                if wanted.is_empty() {
                    continue;
                }
                if let Ok(count) = install_depot_manifests(&steam_root, &wanted) {
                    restored += count;
                    repaired.push(app_id);
                }
            }

            // At most one fetch per sweep, and each app only once per session: packaging a depot
            // upstream takes seconds for a small title and minutes for a large one, so retrying the
            // whole backlog at once would hammer a service that is most likely already struggling.
            // The sweep interval spaces the rest out on its own.
            if let Some(app_id) = never_stored.into_iter().find(|id| !fetch_attempted.contains(id))
                && let Ok(proxy) = ProxyClient::new()
            {
                attempted = Some(app_id);
                if let Ok((count, _)) =
                    copy_depot_manifests_to_cache(&proxy, &steam_root, &store, app_id, None)
                {
                    restored += count;
                    repaired.push(app_id);
                }
            }
            let _ = sender.send((restored, repaired, attempted));
        });
    }

    pub fn poll_manifest_guard(&mut self) {
        let Some(receiver) = self.manifest_guard_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok((restored, repaired, attempted)) => {
                self.manifest_guard_receiver = None;
                if let Some(app_id) = attempted {
                    self.manifest_fetch_attempted.insert(app_id);
                }
                if restored > 0 {
                    self.status = format!(
                        "Steam had dropped {restored} depot manifest(s) for {} game(s) — restored from \
                         Drydock's copy.",
                        repaired.len()
                    );
                    self.status_error = false;
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.manifest_guard_receiver = None,
        }
    }

    pub fn confirm_uncracked_launch(&mut self, app_id: u32) -> bool {
        if self.launch_warned.contains(&app_id) {
            return true;
        }
        let Some(game) = self.settings.installed_games.get(&app_id).cloned() else {
            return true;
        };
        // Prefer the folder the launch exe sits in — that is where the cracker deploys — and fall
        // back to the install root when no exe has been picked yet.
        let folder = self
            .settings
            .launch_paths
            .get(&app_id)
            .map(PathBuf::from)
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(&game.install_dir));
        if crack_deployed_in(&folder) {
            return true;
        }
        self.launch_warned.insert(app_id);
        let name = if game.name.is_empty() {
            self.app_display_name(app_id)
        } else {
            game.name.clone()
        };
        let answer = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Game is not cracked")
            .set_description(format!(
                "\"{name}\" has no crack in its folder, so it will most likely refuse to start — a \
                 game downloaded in Drydock needs one (or an activation, for Denuvo titles).\n\n\
                 Crack it now?"
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if answer == rfd::MessageDialogResult::Yes {
            self.crack_drydock_game(app_id);
            // Cracking runs in the background; the user presses Play again once it reports success.
            return false;
        }
        true
    }
}

impl eframe::App for DrydockApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.image_textures.forget_retired(ui.ctx());
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
        self.poll_unlock_updates();
        self.poll_manifest_guard();
        self.maybe_update_unlocks();
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
            || !self.download_install_receiver.is_empty()
            || self.cloud_download_receiver.is_some()
            || self.cloud_oauth_receiver.is_some()
            || self.unlock_update_receiver.is_some()
            || self.manifest_guard_receiver.is_some()
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
        self.sidebar_nav(ui);
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
                                        Page::SeeAll => self.home_see_all_page(ui),
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
        self.notifications_window(&context);
        self.busy_overlay(&context);
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
    fn format_number_with_commas_formats_correctly() {
        assert_eq!(format_number_with_commas(0), "0");
        assert_eq!(format_number_with_commas(999), "999");
        assert_eq!(format_number_with_commas(1000), "1,000");
        assert_eq!(format_number_with_commas(70616), "70,616");
        assert_eq!(format_number_with_commas(1234567), "1,234,567");
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
