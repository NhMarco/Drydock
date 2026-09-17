use std::fs;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Color32, RichText, Stroke};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;

impl DrydockApp {
    pub fn start_update_check(&mut self, automatic: bool) {
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

    pub fn poll_update(&mut self) {
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

    pub fn updates_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  Updates", icons::UPDATES));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let active_count = (self.settings.auto_update_drydock as usize)
                    + (self.settings.auto_update_unlocks as usize);
                if !self.settings.legacy_update_blocks.is_empty() {
                    status_pill(ui, &format!("⚠ {} Legacy Blocks", self.settings.legacy_update_blocks.len()), AMBER);
                } else if active_count == 2 {
                    status_pill(ui, "● All Auto-Updates Active", VERDIGRIS);
                } else if active_count == 1 {
                    status_pill(ui, "◐ Partial Auto-Updates", ACCENT_SOFT);
                } else {
                    status_pill(ui, "○ Updates Manual", MUTED);
                }
            });
        });
        ui.add_space(18.0);

        // Card 1: Drydock Client Auto-Update
        let previous_auto_update = self.settings.auto_update_drydock;
        let mut auto_update_changed = false;

        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::UPDATES).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("DRYDOCK CLIENT AUTO-UPDATE").size(14.0).strong().color(TEXT));
                    ui.add_space(8.0);
                    status_pill(ui, &format!("v{APP_VERSION}"), ACCENT_SOFT);
                });
                ui.add_space(6.0);
                let channel = if AppUpdater::configured_repository().is_some() {
                    "Verified binary updates downloaded on application launch from your configured release repository."
                } else {
                    "Official release channel with verified SHA-256 binary integrity checks on launch."
                };
                ui.label(RichText::new(channel).size(13.0).color(MUTED));
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    let toggle_changed = toggle_switch(ui, &mut self.settings.auto_update_drydock, ACCENT_SOFT).changed();
                    ui.add_space(8.0);
                    if self.settings.auto_update_drydock {
                        ui.label(RichText::new("Automatic client updates enabled on launch").size(13.0).color(TEXT));
                    } else {
                        ui.label(RichText::new("Automatic client updates disabled").size(13.0).color(MUTED));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let checking = self.update_receiver.is_some();
                        if ui
                            .add_enabled(
                                AppUpdater::can_self_update() && !checking,
                                ghost_button(&format!("{}  CHECK NOW", icons::SEARCH)).compact(),
                            )
                            .on_hover_text("Query the release channel for verified Drydock updates")
                            .clicked()
                        {
                            self.start_update_check(false);
                        }
                        if checking {
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                                ui.add_space(4.0);
                                ui.label(RichText::new("Checking release channel…").size(12.0).color(ACCENT_SOFT));
                            });
                        }
                    });

                    if toggle_changed {
                        auto_update_changed = true;
                    }
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

        // Card 2: Lua & Manifest Auto-Update
        let previous_unlock_updates = self.settings.auto_update_unlocks;
        let mut unlock_updates_changed = false;

        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::SPARKLES).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("LUA & MANIFEST AUTO-UPDATE").size(14.0).strong().color(TEXT));
                    ui.add_space(8.0);
                    status_pill(ui, "Twice Daily", ACCENT_SOFT);
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Games added with the latest version get new Lua scripts and depot manifests twice a day. \
                         Cracked versions and your own custom files are left alone.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    let toggle_changed = toggle_switch(ui, &mut self.settings.auto_update_unlocks, ACCENT_SOFT).changed();
                    ui.add_space(8.0);
                    if self.settings.auto_update_unlocks {
                        ui.label(RichText::new("Automatic Lua and manifest sync active (every 12 hours)").size(13.0).color(TEXT));
                    } else {
                        ui.label(RichText::new("Automatic unlock updates disabled").size(13.0).color(MUTED));
                    }

                    if toggle_changed {
                        unlock_updates_changed = true;
                    }
                });
            });

        if unlock_updates_changed {
            match self.persist_settings() {
                Ok(()) => {
                    // Turning it on checks at the next opportunity instead of in a few minutes.
                    self.last_unlock_update_check = None;
                    self.status = if self.settings.auto_update_unlocks {
                        "Lua and manifest auto update enabled".into()
                    } else {
                        "Lua and manifest auto update disabled".into()
                    };
                    self.status_error = false;
                }
                Err(error) => {
                    self.settings.auto_update_unlocks = previous_unlock_updates;
                    self.status = format!("Auto-update setting could not be saved: {error}");
                    self.status_error = true;
                }
            }
        }

        ui.add_space(16.0);

        // Card 3: Steam Update Architecture / Legacy Blocks Cleanup
        if !self.settings.legacy_update_blocks.is_empty() {
            let legacy_count = self.settings.legacy_update_blocks.len();
            egui::Frame::new()
                .fill(Color32::from_rgb(26, 20, 10))
                .stroke(Stroke::new(1.0, Color32::from_rgb(180, 110, 20)))
                .corner_radius(16)
                .inner_margin(24)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::SHIELD).size(18.0).color(AMBER));
                        ui.add_space(4.0);
                        ui.label(RichText::new("LEGACY STEAM UPDATE BLOCKS").size(14.0).strong().color(AMBER));
                        ui.add_space(8.0);
                        status_pill(ui, &format!("{legacy_count} Locked Manifests"), AMBER);
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Previous versions of Drydock locked Steam .acf manifests to prevent updates. \
                             This fragile practice caused Steam write errors and is no longer needed: \
                             Drydock now uses dedicated install directories and on-demand depot verification. \
                             Click below to lift all legacy read-only locks and restore standard permissions.",
                        )
                        .size(13.0)
                        .color(MUTED),
                    );
                    ui.add_space(16.0);
                    if ui
                        .add(primary_button(&format!("{}  LIFT ALL BLOCKS NOW", icons::CHECK)).compact())
                        .on_hover_text("Remove read-only attributes from all previously locked Steam manifests")
                        .clicked()
                    {
                        self.release_old_update_blocks();
                    }
                });
        } else {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(16)
                .inner_margin(24)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::CHECK).size(18.0).color(VERDIGRIS));
                        ui.add_space(4.0);
                        ui.label(RichText::new("STEAM MANIFEST ARCHITECTURE").size(14.0).strong().color(TEXT));
                        ui.add_space(8.0);
                        status_pill(ui, "● Clean & Unlocked", VERDIGRIS);
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "All Steam manifests are fully unlocked. Drydock protects your games non-intrusively \
                             via isolated directories and custom depot verification, guaranteeing zero Steam client \
                             lock-file conflicts.",
                        )
                        .size(13.0)
                        .color(MUTED),
                    );
                });
        }
    }
}
