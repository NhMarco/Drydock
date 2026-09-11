use std::path::Path;

use drydock_core::*;
use eframe::egui::{self, RichText};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;
use crate::ui::components::*;

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

    pub fn save_settings(&mut self) -> bool {
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
    pub fn self_hosting_panel(&mut self, ui: &mut egui::Ui) {
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

