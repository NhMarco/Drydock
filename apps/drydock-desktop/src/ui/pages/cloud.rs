use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, RichText};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

impl DrydockApp {
    pub fn refresh_cloud_state(&mut self) {
        self.cloud_dll_status = self.steam.root.as_deref().map(cloud::dll_status);
        self.cloud_provider = cloud::current_provider();
    }

    /// Re-scans `<steam>/config/stplug-in` for the App IDs that actually have an unlock Lua.
    ///
    /// One directory listing, kept off the render path. Everything that asks "is this app added?"
    /// reads the cached set via [`Self::is_app_added`].
    pub fn start_cloud_download(&mut self) {
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
    pub fn poll_cloud_download(&mut self) {
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

    pub fn start_cloud_oauth(&mut self, provider: CloudProvider) {
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
    pub fn poll_cloud_oauth(&mut self) {
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

    pub fn cloud_page(&mut self, ui: &mut egui::Ui) {
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

}

