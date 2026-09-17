use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke, Vec2};

use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

fn provider_chip(ui: &mut egui::Ui, label: &str, active: bool, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        SURFACE_RAISED
    } else {
        lerp_color(SURFACE, SURFACE_RAISED, hover)
    };
    let stroke = if active {
        Stroke::new(1.5, ACCENT)
    } else {
        Stroke::new(1.0, lerp_color(BORDER, ACCENT_SOFT, hover))
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        if active { ACCENT } else { TEXT },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

fn cloud_field(ui: &mut egui::Ui, label: &str, hint: &str, value: &mut String, password: bool) {
    ui.label(RichText::new(label).size(12.0).strong().color(ACCENT));
    ui.add_space(4.0);
    ui.add_sized(
        [ui.available_width(), 38.0],
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .password(password)
            .font(FontId::monospace(13.0))
            .margin(egui::Margin::symmetric(12, 9)),
    );
    ui.add_space(10.0);
}

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
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  Cloud Saves", icons::CLOUD));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(current) = self.cloud_provider {
                    status_pill(ui, &format!("⚡ Active: {}", current.display_name()), ACCENT_SOFT);
                    ui.add_space(8.0);
                }
                let status = self.cloud_dll_status.clone().unwrap_or_default();
                if status.installed {
                    status_pill(ui, "● Hook Active", VERDIGRIS);
                } else {
                    status_pill(ui, "○ Hook Inactive", MUTED);
                }
            });
        });
        ui.add_space(18.0);

        // Overview & Caution Banner
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::CLOUD).size(20.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("STEAM CLOUD FOR UNLOCKED GAMES").size(14.0).strong().color(TEXT));
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "CloudRedirect intercepts in-game Steam Cloud requests and routes your save games \
                         to any storage provider you configure. Play across multiple devices with seamless save syncing.",
                    )
                    .size(13.5)
                    .color(MUTED),
                );
                ui.add_space(14.0);

                egui::Frame::new()
                    .fill(Color32::from_rgb(26, 20, 10))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(180, 110, 20)))
                    .corner_radius(8)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        let avail_w = ui.available_width();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icons::SHIELD).size(16.0).color(AMBER));
                            ui.add_space(6.0);
                            let text_w = (avail_w - 36.0).max(100.0);
                            ui.add_sized(
                                [text_w, 0.0],
                                egui::Label::new(
                                    RichText::new(
                                        "EXPERIMENTAL FEATURE · Always back up important save files locally before \
                                         enabling cloud synchronization. Save file conflicts or bad network syncs can overwrite data.",
                                    )
                                    .size(12.5)
                                    .strong()
                                    .color(AMBER),
                                )
                                .wrap(),
                            );
                        });
                    });
            });

        ui.add_space(16.0);

        // CloudRedirect Runtime Hook Card
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::SETTINGS).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("CLOUDREDIRECT RUNTIME HOOK").size(14.0).strong().color(TEXT));
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "A lightweight proxy DLL placed beside steam.exe that intercepts Steam Cloud API calls and redirects save payloads.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(16.0);

                let downloading = self.cloud_download_receiver.is_some();
                if downloading {
                    ui.ctx().request_repaint();
                }

                match self.steam.root.clone() {
                    Some(root) => {
                        let status = self.cloud_dll_status.clone().unwrap_or_default();

                        egui::Frame::new()
                            .fill(SURFACE_RAISED)
                            .stroke(Stroke::new(
                                1.0,
                                if status.installed {
                                    Color32::from_rgb(34, 197, 94)
                                } else {
                                    BORDER
                                },
                            ))
                            .corner_radius(10)
                            .inner_margin(16)
                            .show(ui, |ui| {
                                let (dot_color, title_text, desc_text) = if status.installed {
                                    (
                                        VERDIGRIS,
                                        "Runtime Hook Installed & Ready",
                                        format!("cloud_redirect.dll active in {}", root.display()),
                                    )
                                } else {
                                    (
                                        MUTED,
                                        "Runtime Hook Not Installed",
                                        format!("Hook missing from steam.exe directory ({})", root.display()),
                                    )
                                };

                                let avail_w = ui.available_width();
                                let btn_w = if status.installed { 240.0 } else { 150.0 };
                                let text_w = (avail_w - btn_w - 20.0).max(140.0);

                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("●").size(18.0).color(dot_color));
                                    ui.add_space(6.0);
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(text_w, 0.0),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.label(RichText::new(title_text).size(13.5).strong().color(TEXT));
                                            ui.add(
                                                egui::Label::new(RichText::new(desc_text).size(12.0).color(MUTED))
                                                    .wrap(),
                                            );
                                        },
                                    );

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if status.installed {
                                            if ui
                                                .add_enabled(!downloading, ghost_button(&format!("{}  UNINSTALL", icons::CLOSE)).compact())
                                                .on_hover_text("Remove cloud_redirect.dll from Steam folder")
                                                .clicked()
                                            {
                                                match cloud::remove_dll(&root) {
                                                    Ok(()) => {
                                                        self.status = "CloudRedirect DLL removed".into();
                                                        self.status_error = false;
                                                    }
                                                    Err(error) => {
                                                        self.status = format!("Could not remove DLL: {error}");
                                                        self.status_error = true;
                                                    }
                                                }
                                                self.refresh_cloud_state();
                                            }
                                            ui.add_space(8.0);
                                            if ui
                                                .add_enabled(!downloading, primary_button(&format!("{}  UPDATE HOOK", icons::UPDATES)).compact())
                                                .on_hover_text("Download the latest verified cloud_redirect.dll")
                                                .clicked()
                                            {
                                                self.start_cloud_download();
                                            }
                                        } else if ui
                                            .add_enabled(!downloading, primary_button(&format!("{}  INSTALL HOOK", icons::DOWNLOAD)).compact())
                                            .on_hover_text("Download verified cloud_redirect.dll and deploy next to steam.exe")
                                            .clicked()
                                        {
                                            self.start_cloud_download();
                                        }
                                    });
                                });
                            });

                        if downloading {
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Downloading and verifying cloud_redirect.dll…")
                                        .size(13.0)
                                        .color(ACCENT_SOFT),
                                );
                            });
                        }
                    }
                    None => {
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
                                        RichText::new(
                                            "Steam installation directory not found. Please set your Steam folder in Settings.",
                                        )
                                        .size(13.0)
                                        .strong()
                                        .color(Color32::from_rgb(248, 113, 113)),
                                    );
                                });
                            });
                    }
                }
            });

        ui.add_space(16.0);

        // Storage Provider Configuration Card
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::FOLDER).size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new("STORAGE PROVIDER CONFIGURATION").size(14.0).strong().color(TEXT));
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Choose where your game save files will be stored and synchronized.")
                        .size(13.0)
                        .color(MUTED),
                );
                ui.add_space(16.0);

                let available = ui.available_width();
                let cols = if available >= 560.0 { 3 } else { 2 };
                let spacing = 8.0;
                let chip_w = ((available - spacing * (cols as f32 - 1.0)) / cols as f32).max(120.0);

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(spacing, spacing);
                    let providers = [
                        (CloudProvider::Folder, "Folder / Drive", icons::FOLDER),
                        (CloudProvider::LocalOnly, "Local Staging", icons::CHECK),
                        (CloudProvider::GoogleDrive, "Google Drive", icons::SPARKLES),
                        (CloudProvider::OneDrive, "OneDrive", icons::SPARKLES),
                        (CloudProvider::R2, "Cloudflare R2", icons::CLOUD),
                        (CloudProvider::S3, "Amazon S3", icons::CLOUD),
                    ];
                    for (p, label, icon) in providers {
                        let is_selected = self.cloud.provider == p;
                        if provider_chip(ui, &format!("{icon}  {label}"), is_selected, chip_w) {
                            self.cloud.provider = p;
                        }
                    }
                });

                ui.add_space(18.0);
                ui.separator();
                ui.add_space(16.0);

                match self.cloud.provider {
                    CloudProvider::Folder => {
                        ui.label(RichText::new("SYNC FOLDER PATH").size(12.0).strong().color(ACCENT));
                        ui.add_space(6.0);
                        let field_width = (ui.available_width() - 136.0).max(200.0);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [field_width, 40.0],
                                egui::TextEdit::singleline(&mut self.cloud.folder_path)
                                    .hint_text("C:\\Users\\...\\Google Drive\\SteamSaves  (or mapped network drive)")
                                    .margin(egui::Margin::symmetric(12, 10)),
                            );
                            if ui
                                .add_sized([126.0, 40.0], ghost_button(&format!("{}  BROWSE", icons::FOLDER)))
                                .clicked()
                                && let Some(folder) = rfd::FileDialog::new()
                                    .set_title("Select Cloud Sync Folder")
                                    .pick_folder()
                            {
                                self.cloud.folder_path = folder.display().to_string();
                            }
                        });
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Points to any directory on your computer or a synced cloud storage folder (Dropbox, Google Drive desktop client, Nextcloud, etc.).",
                            )
                            .size(12.5)
                            .color(MUTED),
                        );
                    }
                    CloudProvider::LocalOnly => {
                        ui.label(
                            RichText::new(
                                "Local Only staging suppresses Steam Cloud sync warning dialogs by storing saves in a dedicated local cache directory without attempting remote synchronization.",
                            )
                            .size(13.0)
                            .color(MUTED),
                        );
                        ui.add_space(14.0);
                        ui.label(RichText::new("LOCAL STAGING PATH (OPTIONAL)").size(12.0).strong().color(ACCENT));
                        ui.add_space(6.0);
                        let field_width = (ui.available_width() - 136.0).max(200.0);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [field_width, 40.0],
                                egui::TextEdit::singleline(&mut self.cloud.local_path)
                                    .hint_text("Defaults to <Steam>\\localcloud when left empty")
                                    .margin(egui::Margin::symmetric(12, 10)),
                            );
                            if ui
                                .add_sized([126.0, 40.0], ghost_button(&format!("{}  BROWSE", icons::FOLDER)))
                                .clicked()
                                && let Some(folder) = rfd::FileDialog::new()
                                    .set_title("Select Local Staging Folder")
                                    .pick_folder()
                            {
                                self.cloud.local_path = folder.display().to_string();
                            }
                        });
                    }
                    CloudProvider::R2 => {
                        ui.label(
                            RichText::new(
                                "Cloudflare R2 provides S3-compatible cloud object storage with zero egress fees.",
                            )
                            .size(13.0)
                            .color(MUTED),
                        );
                        ui.add_space(14.0);

                        cloud_field(
                            ui,
                            "ACCOUNT ID",
                            "Cloudflare 32-character hexadecimal account ID",
                            &mut self.cloud.account_id,
                            false,
                        );
                        cloud_field(
                            ui,
                            "ACCESS KEY ID",
                            "R2 Token Access Key ID",
                            &mut self.cloud.access_key_id,
                            false,
                        );
                        cloud_field(
                            ui,
                            "SECRET ACCESS KEY",
                            "R2 Token Secret Access Key",
                            &mut self.cloud.secret_access_key,
                            true,
                        );
                        cloud_field(
                            ui,
                            "BUCKET NAME",
                            "e.g. steam-cloud-saves",
                            &mut self.cloud.bucket,
                            false,
                        );
                        cloud_field(
                            ui,
                            "KEY PREFIX (OPTIONAL)",
                            "e.g. saves/  (leave blank for root)",
                            &mut self.cloud.key_prefix,
                            false,
                        );
                    }
                    CloudProvider::S3 => {
                        ui.label(
                            RichText::new("Connect any AWS S3, MinIO, Wasabi, or S3-compatible storage bucket.")
                                .size(13.0)
                                .color(MUTED),
                        );
                        ui.add_space(14.0);

                        cloud_field(
                            ui,
                            "ENDPOINT URL",
                            "https://s3.amazonaws.com  (or custom endpoint)",
                            &mut self.cloud.endpoint,
                            false,
                        );
                        cloud_field(ui, "REGION", "e.g. us-east-1", &mut self.cloud.region, false);
                        cloud_field(
                            ui,
                            "ACCESS KEY ID",
                            "S3 Access Key ID",
                            &mut self.cloud.access_key_id,
                            false,
                        );
                        cloud_field(
                            ui,
                            "SECRET ACCESS KEY",
                            "S3 Secret Access Key",
                            &mut self.cloud.secret_access_key,
                            true,
                        );
                        cloud_field(
                            ui,
                            "BUCKET NAME",
                            "e.g. my-steam-saves",
                            &mut self.cloud.bucket,
                            false,
                        );
                        cloud_field(
                            ui,
                            "KEY PREFIX (OPTIONAL)",
                            "e.g. saves/  (leave blank for root)",
                            &mut self.cloud.key_prefix,
                            false,
                        );
                    }
                    CloudProvider::GoogleDrive | CloudProvider::OneDrive => {
                        let provider = self.cloud.provider;
                        let in_progress = self.cloud_oauth_receiver.is_some();
                        if in_progress {
                            ui.ctx().request_repaint();
                        }

                        egui::Frame::new()
                            .fill(SURFACE_RAISED)
                            .stroke(Stroke::new(1.0, BORDER))
                            .corner_radius(12)
                            .inner_margin(20)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(icons::SPARKLES).size(22.0).color(ACCENT));
                                    ui.add_space(8.0);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(format!("{} OAuth Authentication", provider.display_name()))
                                                .size(14.0)
                                                .strong()
                                                .color(TEXT),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "Sign in securely via your web browser. Drydock will automatically obtain and persist the access tokens required by cloud_redirect.dll.",
                                            )
                                            .size(12.5)
                                            .color(MUTED),
                                        );
                                    });
                                });
                                ui.add_space(16.0);

                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            !in_progress,
                                            primary_button(&format!("{}  SIGN IN WITH BROWSER", icons::PLAY))
                                                .min_size(Vec2::new(220.0, 40.0)),
                                        )
                                        .on_hover_text("Open official sign-in page in default browser")
                                        .clicked()
                                    {
                                        self.start_cloud_oauth(provider);
                                    }
                                    if in_progress {
                                        ui.add_space(12.0);
                                        ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                                        ui.add_space(4.0);
                                        ui.label(
                                            RichText::new("Waiting for browser authentication to complete…")
                                                .size(13.0)
                                                .color(ACCENT_SOFT),
                                        );
                                    }
                                });
                            });
                    }
                }

                if let Some(settings) = self.cloud.to_settings() {
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                primary_button(&format!("{}  SAVE CLOUD CONFIGURATION", icons::CHECK))
                                    .min_size(Vec2::new(260.0, 42.0)),
                            )
                            .on_hover_text("Write config.json and credentials so cloud_redirect.dll uses this provider")
                            .clicked()
                        {
                            match cloud::write_settings(&settings) {
                                Ok(()) => {
                                    self.status = format!(
                                        "Cloud configuration saved for {}. Restart Steam to apply.",
                                        self.cloud.provider.display_name()
                                    );
                                    self.status_error = false;
                                }
                                Err(error) => {
                                    self.status = format!("Could not save cloud configuration: {error}");
                                    self.status_error = true;
                                }
                            }
                            self.refresh_cloud_state();
                        }

                        ui.add_space(12.0);
                        ui.label(RichText::new("Requires restarting Steam to apply changes.").size(12.5).color(MUTED));
                    });
                }
            });
        ui.add_space(28.0);
    }

}

