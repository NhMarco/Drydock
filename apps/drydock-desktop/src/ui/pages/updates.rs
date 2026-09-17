use std::fs;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};

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
                let protected_count = self
                    .manifests
                    .iter()
                    .filter(|m| {
                        let pref = self
                            .settings
                            .steam_updates_enabled
                            .get(&m.app_id)
                            .copied()
                            .or_else(|| updates_enabled(&m.manifest_path).ok())
                            .unwrap_or(true);
                        !pref
                    })
                    .count();
                if protected_count > 0 {
                    status_pill(ui, &format!("🛡 {protected_count} Protected Games"), DANGER);
                } else {
                    status_pill(ui, "● All Updates Allowed", ACCENT_SOFT);
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
                        ui.label(RichText::new("Automatic updates enabled on launch").size(13.0).color(TEXT));
                    } else {
                        ui.label(RichText::new("Automatic updates disabled").size(13.0).color(MUTED));
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

        // Caution Banner for Steam Updates
        egui::Frame::new()
            .fill(Color32::from_rgb(26, 20, 10))
            .stroke(Stroke::new(1.0, Color32::from_rgb(180, 110, 20)))
            .corner_radius(12)
            .inner_margin(16)
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::SHIELD).size(18.0).color(AMBER));
                    ui.add_space(8.0);
                    let text_w = (avail_w - 40.0).max(100.0);
                    ui.add_sized(
                        [text_w, 0.0],
                        egui::Label::new(
                            RichText::new(
                                "STEAM UPDATE PROTECTION · Automatic Steam updates can overwrite game executables, \
                                 reverting custom fixes, DLC unlocks, and offline activations. \
                                 Toggle updates to 'BLOCKED' on any modified title to preserve your files.",
                            )
                            .size(12.5)
                            .strong()
                            .color(AMBER),
                        )
                        .wrap(),
                    );
                });
            });

        ui.add_space(16.0);

        // Section header for game manifests
        ui.horizontal(|ui| {
            ui.label(RichText::new(icons::FOLDER).size(16.0).color(ACCENT));
            ui.add_space(4.0);
            ui.label(RichText::new("STEAM GAME MANIFESTS").size(13.0).strong().color(TEXT));
            ui.add_space(6.0);
            status_pill(ui, &format!("{} games found", self.manifests.len()), MUTED);
        });
        ui.add_space(10.0);

        let mut pending_change = None;

        if self.manifests.is_empty() {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(16)
                .inner_margin(32)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(icons::FOLDER).size(32.0).color(MUTED));
                        ui.add_space(8.0);
                        ui.label(RichText::new("No Steam game manifests detected").size(15.0).strong().color(TEXT));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Verify that your Steam installation directory is set correctly in Settings.")
                                .size(13.0)
                                .color(MUTED),
                        );
                    });
                });
        }

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
                .stroke(Stroke::new(
                    1.0,
                    if !preference {
                        Color32::from_rgb(180, 60, 60)
                    } else {
                        BORDER
                    },
                ))
                .corner_radius(14)
                .inner_margin(18)
                .show(ui, |ui| {
                    let avail_w = ui.available_width();
                    let right_w = 230.0;
                    let left_w = (avail_w - right_w - 16.0).max(120.0);

                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(Vec2::new(left_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&manifest.name).size(15.0).strong().color(TEXT));
                                ui.add_space(6.0);
                                status_pill(ui, &format!("APP {}", manifest.app_id), ACCENT_SOFT);
                            });
                            ui.add_space(3.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(manifest.manifest_path.display().to_string())
                                        .size(12.0)
                                        .font(FontId::monospace(11.5))
                                        .color(MUTED),
                                )
                                .truncate(),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                                status_pill(ui, "🛡 UPDATES BLOCKED", DANGER);
                            }
                        });
                    });
                });
            ui.add_space(8.0);
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
                                    format!("Update preference was not saved and was rolled back: {error}")
                                }
                                Err(rollback_error) => {
                                    format!("Update preference was not saved and rollback failed: {error}; {rollback_error}")
                                }
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
    }

}

