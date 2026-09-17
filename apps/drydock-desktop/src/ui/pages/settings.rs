use drydock_core::depot::MAXIMUM_VERIFY_THREADS;
use drydock_core::*;
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use std::path::Path;

use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

impl DrydockApp {
    pub fn validate_steam_draft(&mut self) -> SteamDirValidation {
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

    /// The configured games folder, if one is set — where a fresh Drydock download installs. `None`
    /// falls back to Steam's `steamapps\common`.
    pub fn games_directory(&self) -> Option<std::path::PathBuf> {
        path_if_present(&self.settings.games_directory).map(Path::to_path_buf)
    }

    pub fn save_settings(&mut self) -> bool {
        self.settings.steam_directory = self.steam_directory_draft.trim().to_owned();
        self.settings.games_directory = self.games_directory_draft.trim().to_owned();
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
    pub fn write_settings(&self) -> Result<(), String> {
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
    pub fn persist_settings(&mut self) -> Result<(), String> {
        let result = self.write_settings();
        if let Err(error) = &result {
            self.status = ellipsize(&format!("Your change could not be saved: {error}"), 160);
            self.status_error = true;
        }
        result
    }

    /// Accepts the loss of an unreadable settings file and re-enables saving, starting from whatever
    /// is currently in memory (the defaults). The quarantined copy stays on disk either way.
    pub fn discard_broken_settings(&mut self) {
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
    pub fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  Settings", icons::SETTINGS));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.steam.root.is_some() {
                    status_pill(ui, "● Steam Linked", VERDIGRIS);
                } else {
                    status_pill(ui, "○ Steam Unset", MUTED);
                }
                ui.add_space(6.0);
                if self.games_directory_draft.trim().is_empty() {
                    status_pill(ui, "Library: Steam Default", ACCENT_SOFT);
                } else {
                    status_pill(ui, "Library: Custom Folder", VERDIGRIS);
                }
            });
        });
        ui.add_space(18.0);

        // Card 1: Steam Directory & Data Storage
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FOLDER).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("STEAM DIRECTORY").size(14.0).strong().color(TEXT));
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Point Drydock at your primary Steam installation folder. Drydock scans installed games \
                         and configures Steam integrations here.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(14.0);

                ui.add_sized(
                    [ui.available_width(), 40.0],
                    egui::TextEdit::singleline(&mut self.steam_directory_draft)
                        .hint_text("C:\\Program Files (x86)\\Steam  (or custom Steam folder)")
                        .font(FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(12, 10)),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    match self.validate_steam_draft() {
                        SteamDirValidation::Empty => {
                            ui.label(RichText::new("Enter the path to your Steam folder above").size(12.5).color(MUTED));
                        }
                        SteamDirValidation::NotFound => {
                            ui.label(RichText::new(format!("{}  Folder does not exist", icons::CLOSE)).size(12.5).color(DANGER));
                        }
                        SteamDirValidation::MissingExecutable => {
                            ui.label(
                                RichText::new(format!("{}  Not a Steam folder — missing steam.exe / steam.sh", icons::CLOSE))
                                    .size(12.5)
                                    .color(DANGER),
                            );
                        }
                        SteamDirValidation::MissingSteamapps => {
                            ui.label(
                                RichText::new(format!("{}  Steam executable found, but steamapps folder is missing", icons::SHIELD))
                                    .size(12.5)
                                    .color(AMBER),
                            );
                        }
                        SteamDirValidation::Valid => {
                            ui.label(RichText::new(format!("{}  Steam installation verified", icons::CHECK)).size(12.5).color(VERDIGRIS));
                        }
                    }
                });
                ui.add_space(14.0);

                ui.horizontal(|ui| {
                    if ui
                        .add(primary_button(&format!("{}  SAVE AND RELOAD", icons::CHECK)).compact())
                        .on_hover_text("Save the Steam directory and reload installed games")
                        .clicked()
                        && self.save_settings()
                    {
                        self.refresh_steam();
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(ghost_button(&format!("{}  AUTO-DETECT", icons::SEARCH)).compact())
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
                    ui.add_space(8.0);
                    if ui
                        .add(ghost_button(&format!("{}  BROWSE", icons::FOLDER)).compact())
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
                ui.add_space(14.0);

                ui.label(RichText::new("DATA STORAGE DIRECTORY").size(12.0).strong().color(ACCENT));
                ui.add_space(4.0);
                let settings_path = self.paths.settings_dir().display().to_string();
                ui.label(
                    RichText::new(settings_path)
                        .size(13.0)
                        .font(FontId::monospace(13.0))
                        .color(MUTED),
                );
            });

        ui.add_space(16.0);

        // Card 2: Games Installation Folder
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FOLDER).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("GAMES INSTALLATION FOLDER").size(14.0).strong().color(TEXT));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.games_directory_draft.trim().is_empty() {
                            status_pill(ui, "Steam Library Default", ACCENT_SOFT);
                        } else {
                            status_pill(ui, "Custom Folder", VERDIGRIS);
                        }
                    });
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Where Drydock installs the games it downloads itself. Leaving this empty keeps the default \
                         behavior (Steam's own library), so an existing setup is unaffected until you pick a folder.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(14.0);

                ui.add_sized(
                    [ui.available_width(), 40.0],
                    egui::TextEdit::singleline(&mut self.games_directory_draft)
                        .hint_text("Install folder for games downloaded in Drydock (empty = Steam library)")
                        .font(FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(12, 10)),
                );
                ui.add_space(8.0);

                let draft = self.games_directory_draft.trim().to_owned();
                ui.horizontal(|ui| {
                    if draft.is_empty() {
                        let fallback = self.steam.root.as_ref().map_or_else(
                            || "Steam folder not set — pick a games folder here".to_owned(),
                            |root| root.join("steamapps").join("common").display().to_string(),
                        );
                        ui.label(
                            RichText::new(format!("Using Steam's library: {fallback}"))
                                .size(12.5)
                                .color(MUTED),
                        );
                    } else if Path::new(&draft).is_dir() {
                        ui.label(
                            RichText::new(format!("{}  Folder exists and ready", icons::CHECK))
                                .size(12.5)
                                .color(VERDIGRIS),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("{}  Folder does not exist yet — it will be created on first download", icons::SHIELD))
                                .size(12.5)
                                .color(AMBER),
                        );
                    }
                });
                ui.add_space(14.0);

                ui.horizontal(|ui| {
                    if ui
                        .add(primary_button(&format!("{}  SAVE", icons::CHECK)).compact())
                        .on_hover_text("Save the folder new downloads install into")
                        .clicked()
                    {
                        self.save_settings();
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(ghost_button(&format!("{}  BROWSE", icons::FOLDER)).compact())
                        .on_hover_text("Open a file picker to select the games folder")
                        .clicked()
                    {
                        let mut dialog = rfd::FileDialog::new().set_title("Select games folder");
                        let current = Path::new(self.games_directory_draft.trim());
                        if current.is_dir() {
                            dialog = dialog.set_directory(current);
                        }
                        if let Some(folder) = dialog.pick_folder() {
                            self.games_directory_draft = folder.display().to_string();
                            self.status = "Games folder selected. Save to apply it.".into();
                            self.status_error = false;
                        }
                    }
                    if !self.games_directory_draft.trim().is_empty() {
                        ui.add_space(8.0);
                        if ui
                            .add(ghost_button(&format!("{}  USE STEAM LIBRARY", icons::UPDATES)).compact())
                            .on_hover_text("Clear custom folder and install into Steam's library again")
                            .clicked()
                        {
                            self.games_directory_draft.clear();
                            self.save_settings();
                        }
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Only applies to new downloads. A game Steam already has installed is always \
                         updated where it is.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
            });

        ui.add_space(16.0);

        // Card 3: Steam Service Integration
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                let service_busy = self.service_receiver.is_some();
                let restart_enabled = self.background_action.is_none();

                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::SHIELD).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("STEAM SERVICE INTEGRATION").size(14.0).strong().color(TEXT));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(status) = self.service_status.as_ref() {
                            let (label, color) = match status.state {
                                SteamServiceState::Current => ("● CURRENT", VERDIGRIS),
                                SteamServiceState::UpdateAvailable => ("▲ UPDATE AVAILABLE", ACCENT_SOFT),
                                SteamServiceState::NotInstalled => ("○ NOT INSTALLED", MUTED),
                                SteamServiceState::Error => ("⚠ ATTENTION", DANGER),
                            };
                            status_pill(ui, label, color);
                        } else if service_busy {
                            status_pill(ui, "⏳ CHECKING…", MUTED);
                        } else {
                            status_pill(ui, "○ UNKNOWN", MUTED);
                        }
                    });
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "The Steam Service enables seamless game unlocks, depot manifests injection, \
                         and hook management without needing manual binary edits.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(14.0);

                let Some(_root) = &self.steam.root.clone() else {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(32, 14, 14))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(220, 38, 38)))
                        .corner_radius(8)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icons::CLOSE).size(16.0).color(Color32::from_rgb(220, 38, 38)));
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Steam directory not set — configure and save your Steam folder above first.")
                                        .size(13.0)
                                        .strong()
                                        .color(Color32::from_rgb(248, 113, 113)),
                                );
                            });
                        });
                    return;
                };

                let is_current = matches!(
                    self.service_status.as_ref().map(|s| s.state),
                    Some(SteamServiceState::Current)
                );
                let installed = self.service_status.as_ref().is_some_and(|s| s.state != SteamServiceState::NotInstalled);
                let message = self.service_status.as_ref().map(|s| s.message.clone()).unwrap_or_default();
                let primary_label = self.service_status.as_ref()
                    .map_or("INSTALL SERVICE".to_string(), |s| s.action_text().to_uppercase());

                if !message.is_empty() {
                    ui.add(egui::Label::new(RichText::new(message).size(13.0).color(MUTED)).wrap());
                    ui.add_space(10.0);
                }

                ui.horizontal(|ui| {
                    if !is_current {
                        if ui
                            .add_enabled(
                                !service_busy,
                                primary_button(&format!("{}  {primary_label}", icons::DOWNLOAD)).compact(),
                            )
                            .clicked()
                        {
                            self.install_steam_service();
                        }
                        ui.add_space(8.0);
                    }
                    if installed {
                        if ui
                            .add_enabled(
                                !service_busy,
                                ghost_button(&format!("{}  REINSTALL", icons::UPDATES)).compact(),
                            )
                            .on_hover_text("Download and reinstall the Steam Service files")
                            .clicked()
                        {
                            self.install_steam_service();
                        }
                        ui.add_space(8.0);
                        if ui
                            .add_enabled(
                                !service_busy,
                                ghost_button(&format!("{}  UNINSTALL", icons::CLOSE)).compact(),
                            )
                            .on_hover_text("Remove the Steam Service files and restart Steam")
                            .clicked()
                        {
                            self.uninstall_steam_service();
                        }
                        ui.add_space(8.0);
                    }
                    if ui
                        .add_enabled(
                            restart_enabled && !service_busy,
                            ghost_button(&format!("{}  RESTART STEAM", icons::PLAY)).compact(),
                        )
                        .on_hover_text("Stop and restart the Steam client")
                        .clicked()
                    {
                        self.restart_steam_in_background();
                    }
                });
            });

        ui.add_space(16.0);

        // Card 3: Downloads & Catalog
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::DOWNLOAD).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("DOWNLOADS & CATALOG CONFIGURATION").size(14.0).strong().color(TEXT));
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Configure concurrent chunk streams, download speed limits, and game catalog updates.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(16.0);

                let avail_w = ui.available_width();
                let col_w = ((avail_w - 24.0) / 2.0).max(260.0);

                ui.horizontal_top(|ui| {
                    // Left Column: Download Limits
                    ui.allocate_ui_with_layout(Vec2::new(col_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.label(RichText::new("PARALLEL CONNECTIONS").size(12.0).strong().color(ACCENT));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Maximum concurrent chunk streams per download (default 16).")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.add_space(6.0);

                        let mut connections = match self.settings.max_download_connections {
                            0 => Settings::DEFAULT_DOWNLOAD_CONNECTIONS,
                            n => n.clamp(1, 32),
                        };
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut connections, 1..=32).text("connections"));
                        });
                        if connections != self.settings.max_download_connections {
                            self.settings.max_download_connections = connections;
                            self.status_error = self.persist_settings().is_err();
                        }

                        ui.add_space(16.0);
                        ui.label(RichText::new("BANDWIDTH LIMIT").size(12.0).strong().color(ACCENT));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Throttle maximum download speed. Set to 0 for unlimited.")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.add_space(6.0);

                        let mut mbps = self.settings.max_download_mbps;
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut mbps)
                                    .range(0..=100_000)
                                    .speed(1.0)
                                    .suffix(" MB/s"),
                            );
                            ui.add_space(6.0);
                            if mbps == 0 {
                                status_pill(ui, "UNLIMITED", VERDIGRIS);
                            } else {
                                status_pill(ui, &format!("Capped at {mbps} MB/s"), ACCENT_SOFT);
                            }
                        });
                        if mbps != self.settings.max_download_mbps {
                            self.settings.max_download_mbps = mbps;
                            self.status_error = self.persist_settings().is_err();
                        }

                        ui.add_space(16.0);
                        let mut verify_threads = self.settings.verify_threads.min(MAXIMUM_VERIFY_THREADS);
                        ui.label(RichText::new("VERIFY THREADS").size(12.0).strong().color(ACCENT))
                            .on_hover_text(VERIFY_THREADS_HOVER);
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("How many files a verify reads at once (Auto uses 1 on HDD, several on SSD).")
                                .size(12.0)
                                .color(MUTED),
                        )
                        .on_hover_text(VERIFY_THREADS_HOVER);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Slider::new(&mut verify_threads, 0..=MAXIMUM_VERIFY_THREADS)
                                    .text("threads")
                                    .custom_formatter(|value, _| {
                                        if value < 0.5 {
                                            "Auto".to_owned()
                                        } else {
                                            format!("{value:.0}")
                                        }
                                    }),
                            )
                            .on_hover_text(VERIFY_THREADS_HOVER);
                        });
                        if verify_threads != self.settings.verify_threads {
                            self.settings.verify_threads = verify_threads;
                            self.status_error = self.persist_settings().is_err();
                        }
                    });

                    ui.add_space(24.0);

                    // Right Column: Catalog Refresh
                    ui.allocate_ui_with_layout(Vec2::new(col_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.label(RichText::new("GAME CATALOG SYNC").size(12.0).strong().color(ACCENT));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("The full catalog is cached for 24 hours. Force a refresh to fetch new listings.")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("● {} games indexed", self.catalog.len())).size(13.5).strong().color(TEXT));
                        });
                        ui.add_space(12.0);
                        let refreshing = self.catalog_receiver.is_some();
                        if ui
                            .add_enabled(
                                !refreshing,
                                ghost_button(&format!("{}  FORCE REFRESH GAME LIST", icons::UPDATES)).compact(),
                            )
                            .on_hover_text("Re-download the complete game catalog now")
                            .clicked()
                        {
                            self.force_catalog_refresh();
                        }
                        if refreshing {
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                                ui.add_space(4.0);
                                ui.label(RichText::new("Syncing game catalog…").size(12.0).color(ACCENT_SOFT));
                            });
                        }
                    });
                });
            });

        ui.add_space(16.0);

        // Card 4: Cache & Storage
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::TOOLS).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("CACHE & STORAGE").size(14.0).strong().color(TEXT));
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Delete cached store artwork, game catalog, and Denuvo lists. \
                         Safe to clear anytime; all assets re-download automatically while settings and activations remain intact.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(14.0);

                let cache_path = self.paths.cache_dir().display().to_string();
                ui.label(RichText::new("CACHE DIRECTORY").size(12.0).strong().color(ACCENT));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(cache_path)
                        .size(13.0)
                        .font(FontId::monospace(13.0))
                        .color(MUTED),
                );
                ui.add_space(14.0);

                if ui
                    .add(ghost_button(&format!("{}  CLEAR CACHE", icons::CLOSE)).compact())
                    .on_hover_text("Delete cached artwork, game lists, and temporary data")
                    .clicked()
                {
                    self.clear_drydock_cache();
                }
            });

        ui.add_space(16.0);

        // Card 5: Proxy & Self-Hosting
        self.self_hosting_panel(ui);

        // Card 6: Broken Settings File Alert (Conditional)
        if self.settings_read_only {
            ui.add_space(16.0);
            egui::Frame::new()
                .fill(Color32::from_rgb(32, 20, 10))
                .stroke(Stroke::new(1.0, AMBER))
                .corner_radius(16)
                .inner_margin(24)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::SHIELD).size(18.0).color(AMBER));
                        ui.add_space(4.0);
                        ui.label(RichText::new("BROKEN SETTINGS FILE DETECTED").size(14.0).strong().color(AMBER));
                    });
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Your settings file could not be read on startup. Drydock initialized with defaults \
                                 and is preventing file writes to protect your original library from being replaced. \
                                 The original unreadable file is preserved beside it.",
                            )
                            .size(13.0)
                            .color(TEXT),
                        )
                        .wrap(),
                    );
                    ui.add_space(14.0);
                    if ui
                        .add(ghost_button(&format!("{}  DISCARD BROKEN SETTINGS", icons::CLOSE)).compact())
                        .on_hover_text("Start a fresh settings file from current defaults. The unreadable file remains on disk.")
                        .clicked()
                    {
                        self.discard_broken_settings();
                    }
                });
        }
    }

    /// Settings panel for pointing this build at a self-hosted proxy or a fork's release channel.
    pub fn self_hosting_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::CLOUD).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("PROXY & SELF-HOSTING OVERRIDES")
                            .size(14.0)
                            .strong()
                            .color(TEXT),
                    );
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Leave these fields empty to use official defaults. Fill them in to connect \
                         Drydock to a private proxy or your own release channel.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(16.0);

                let mut changed = false;
                let mut field =
                    |ui: &mut egui::Ui, label: &str, hint: &str, value: &mut String, secret: bool| {
                        ui.label(RichText::new(label).size(12.0).strong().color(ACCENT));
                        ui.add_space(4.0);
                        let edit = egui::TextEdit::singleline(value)
                            .hint_text(hint)
                            .password(secret)
                            .font(FontId::monospace(13.0))
                            .margin(egui::Margin::symmetric(12, 10));
                        if ui.add_sized([ui.available_width(), 40.0], edit).changed() {
                            changed = true;
                        }
                        ui.add_space(12.0);
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
                    "owner/repo  (leave empty unless running a custom release repository)",
                    &mut self.settings.update_repository,
                    false,
                );

                if changed {
                    // Apply immediately so the next request uses the new address — no restart needed.
                    self.settings.apply_config_overrides();
                    let _ = self.persist_settings();
                }

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(14.0);

                ui.label(
                    RichText::new("RESOLVED CONFIGURATION")
                        .size(12.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.add_space(8.0);

                for entry in drydock_core::describe_config() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{}:", entry.name))
                                .size(13.0)
                                .strong()
                                .color(MUTED),
                        );
                        ui.label(
                            RichText::new(&entry.value)
                                .size(13.0)
                                .font(FontId::monospace(12.5))
                                .color(TEXT),
                        );
                        let (source_label, source_color) = match entry.source {
                            ConfigSource::Unset => ("Unset", DANGER),
                            ConfigSource::Environment => ("Env Override", ACCENT_SOFT),
                            ConfigSource::UserSettings => ("User Settings", VERDIGRIS),
                            ConfigSource::BuildDefault => ("Built-in", MUTED),
                        };
                        status_pill(ui, source_label, source_color);
                    });
                    ui.add_space(4.0);
                }

                ui.add_space(6.0);
                ui.label(
                    RichText::new("Run `Drydock --config` from terminal for the same diagnostic report.")
                        .size(12.0)
                        .color(MUTED),
                );
            });
    }

    /// Deletes the on-disk cache and drops the in-memory caches derived from it so they refill.
    pub fn clear_drydock_cache(&mut self) {
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
}
