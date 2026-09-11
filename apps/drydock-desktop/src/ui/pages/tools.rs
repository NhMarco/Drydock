use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, RichText};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;

pub fn build_emu_crack(
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

impl DrydockApp {
    pub fn load_language_from(&mut self, directory: PathBuf) {
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
    pub fn crack_drydock_game(&mut self, app_id: u32) {
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
    /// The Tools tab: a game-folder language changer. Pick the game (or repack) folder; Drydock scans
    /// its subfolders for the Steam-settings language files, lists the supported languages, and
    /// writes the chosen one. Greyed out with an explanation on non-Windows builds is not needed —
    /// language files are cross-platform — but the picker is always available here.
    pub fn tools_page(&mut self, ui: &mut egui::Ui) {
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
    pub fn emu_template_panel(&mut self, ui: &mut egui::Ui) {
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
    pub fn start_emu_crack(&mut self, output: EmuOutput) {
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
    pub fn start_emu_toolchain_refresh(&mut self) {
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

    pub fn poll_emu_template(&mut self) {
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
    pub fn apply_tools_language(&mut self) {
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
}

