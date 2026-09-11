use std::fs;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Align, Layout, RichText, Stroke};

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

}

