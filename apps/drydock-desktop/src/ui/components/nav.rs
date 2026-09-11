
use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::helpers::*;
use crate::ui::widgets::*;

pub fn sidebar_link(ui: &mut egui::Ui, icon: &str, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(11.5);
    let text = if icon.is_empty() {
        label.to_string()
    } else {
        format!("{icon}  {label}")
    };
    let galley = ui.painter().layout_no_wrap(text, font, TEXT);
    let size = Vec2::new(ui.available_width(), 36.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());

    let fill = if active {
        Color32::from_rgba_unmultiplied(0, 225, 250, 26)
    } else {
        lerp_color(Color32::TRANSPARENT, SURFACE_RAISED, hover)
    };
    let stroke = if active {
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.85))
    } else {
        Stroke::new(1.0, lerp_color(Color32::TRANSPARENT, BORDER, hover))
    };

    ui.painter().rect(rect, 8, fill, stroke, egui::StrokeKind::Inside);

    if active {
        let indicator = egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
        ui.painter().rect_filled(indicator, egui::CornerRadius::same(2), ACCENT);
    }

    let text_color = if active {
        ACCENT_SOFT
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    let text_pos = egui::pos2(
        rect.left() + 14.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(text_pos, galley, text_color);

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A left-arrow back button used to return from a detail page to the list it was opened from.

impl DrydockApp {
    pub fn sidebar_nav(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("sidebar_nav")
            .exact_size(230.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(16, 18))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(root, |ui| {
                // Header (Pinned at Top): Logo, Title & Search
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 14.0, ACCENT_DEEP);
                    egui::Image::new(egui::include_image!("../../../../../assets/app-icon.png"))
                        .corner_radius(14)
                        .paint_at(ui, rect);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Drydock")
                            .size(18.0)
                            .strong()
                            .color(TEXT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("v{APP_VERSION}")).size(9.5).color(MUTED));
                    });
                });

                ui.add_space(14.0);

                // Search input pill
                let before = self.search.clone();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search…")
                        .desired_width(ui.available_width())
                        .margin(egui::Margin {
                            left: 30,
                            right: 10,
                            top: 7,
                            bottom: 7,
                        }),
                );
                let center = egui::pos2(response.rect.left() + 15.0, response.rect.center().y);
                let radius = 4.5;
                let painter = ui.painter();
                painter.circle_stroke(center, radius, Stroke::new(1.5, MUTED));
                let d = radius * std::f32::consts::FRAC_1_SQRT_2;
                painter.line_segment(
                    [
                        egui::pos2(center.x + d, center.y + d),
                        egui::pos2(center.x + d + 3.0, center.y + d + 3.0),
                    ],
                    Stroke::new(1.5, MUTED),
                );
                if response.changed() && self.search != before && !self.search.trim().is_empty() {
                    self.page = Page::Home;
                }

                ui.add_space(16.0);

                // Bottom section (Pinned at Bottom)
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Developed with ♥ for gamers")
                            .size(9.5)
                            .color(MUTED),
                    );
                    ui.add_space(6.0);

                    // Notifications summary tile
                    let notes = self.collect_notifications();
                    if let Some(note) = notes.first() {
                        egui::Frame::new()
                            .fill(SURFACE)
                            .stroke(Stroke::new(1.0, BORDER))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                                    ui.painter().circle_filled(dot.center(), 3.5, note.accent);
                                    ui.add_space(4.0);
                                    ui.add(egui::Label::new(
                                        RichText::new(&note.title).size(10.5).color(TEXT)
                                    ).truncate());
                                });
                            });
                        ui.add_space(6.0);
                    }

                    // Downloads Status Pill button at bottom of sidebar
                    let (dl_label, _dl_color) = self.download_status_label();
                    if ui.add(ghost_button(dl_label).min_size(Vec2::new(ui.available_width(), 32.0))).clicked() {
                        self.page = Page::Downloads;
                    }
                    ui.add_space(10.0);

                    // Middle Section (Scrollable Navigation Area)
                    egui::ScrollArea::vertical()
                        .id_salt("sidebar_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                // STOREFRONT section
                                ui.label(RichText::new("STOREFRONT").size(9.5).strong().color(MUTED));
                                ui.add_space(6.0);
                                for (page, icon, label) in [
                                    (Page::Home, "🛍", "STORE"),
                                    (Page::Library, "📚", "LIBRARY"),
                                    (Page::Downloads, "📥", "DOWNLOADS"),
                                ] {
                                    if sidebar_link(ui, icon, label, self.page == page).clicked() {
                                        self.page = page;
                                    }
                                    ui.add_space(4.0);
                                }

                                ui.add_space(16.0);

                                // SERVICES & TOOLS section
                                ui.label(RichText::new("SERVICES & TOOLS").size(9.5).strong().color(MUTED));
                                ui.add_space(6.0);
                                for (page, icon, label) in [
                                    (Page::Activation, "🔑", "ACTIVATION"),
                                    (Page::Tools, "🛠", "TOOLS"),
                                    (Page::Cloud, "☁", "CLOUD"),
                                ] {
                                    if sidebar_link(ui, icon, label, self.page == page).clicked() {
                                        self.page = page;
                                    }
                                    ui.add_space(4.0);
                                }

                                ui.add_space(16.0);

                                // SYSTEM section
                                ui.label(RichText::new("SYSTEM").size(9.5).strong().color(MUTED));
                                ui.add_space(6.0);
                                for (page, icon, label) in [
                                    (Page::Settings, "⚙", "SETTINGS"),
                                    (Page::Updates, "🔄", "UPDATES"),
                                    (Page::Guide, "❓", "HELP"),
                                ] {
                                    if sidebar_link(ui, icon, label, self.page == page).clicked() {
                                        self.page = page;
                                    }
                                    ui.add_space(4.0);
                                }
                            });
                        });
                });
            });
    }

    /// A slim status strip pinned to the bottom of the window: the single most relevant live
    /// notification on the left (Steam/Service/conflict state or the latest activity), the app
    /// version on the right. Replaces the sidebar footer from the old layout.
    /// The Steam-style Downloads bar: shown above the status bar while a depot download or verify is
    /// running (or just finished), with the game name, a progress bar, and Cancel/Dismiss.
    /// The centred download status shown in the bottom bar. Always present (so the Downloads page is
    /// one click away), reading "Downloads" at rest and reflecting the active job otherwise. Per-job
    /// detail (name, %, speed) lives only on the Downloads page.
    pub fn download_status_label(&self) -> (&'static str, Color32) {
        if self.download_running() {
            ("Downloads active", ACCENT)
        } else if self.download_paused {
            ("Downloads paused", DANGER)
        } else if !self.settings.download_queue.is_empty() {
            ("Downloads queued", AMBER)
        } else {
            ("Downloads", MUTED)
        }
    }

    /// The full Downloads page, Steam-style: a hero banner of the current game, live network/peak
    /// speed tiles, the download + install/verify progress bars, and the (currently single-job) queue.
    #[allow(dead_code)]
    pub fn status_bar(&mut self, root: &mut egui::Ui) {
        let (download_label, download_accent) = self.download_status_label();
        let mut open_downloads = false;
        egui::Panel::bottom("status_bar")
            .exact_size(STATUS_BAR_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(18, 0))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(root, |ui| {
                let bar = ui.max_rect();
                // Centred download status, painted on top of the bar (painting, not a widget, so it
                // doesn't consume the panel's layout space). Always shown so the Downloads page is a
                // click away; brightens on hover.
                let hovered = ui.rect_contains_pointer(egui::Rect::from_center_size(
                    bar.center(),
                    Vec2::new(150.0, bar.height()),
                ));
                let color = if hovered {
                    lerp_color(download_accent, TEXT, 0.5)
                } else {
                    download_accent
                };
                let text_rect = ui.painter().text(
                    bar.center(),
                    egui::Align2::CENTER_CENTER,
                    download_label,
                    FontId::proportional(11.5),
                    color,
                );
                let response = ui.interact(text_rect, ui.id().with("dl_status"), Sense::click());
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if response.clicked() {
                    open_downloads = true;
                }
                ui.horizontal_centered(|ui| {
                    let notes = self.collect_notifications();
                    if let Some(note) = notes.first() {
                        // Keep the left status clear of the centred download label: cap the title so a
                        // long message (e.g. a crack summary) ellipsises instead of running into it.
                        let (dot, _) = ui.allocate_exact_size(Vec2::new(9.0, 9.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, note.accent);
                        ui.add_space(4.0);
                        let title = ellipsize(&note.title, 82);
                        let title_label = ui.label(RichText::new(&title).size(11.0).color(TEXT));
                        if title != note.title {
                            title_label.on_hover_text(&note.title);
                        }
                        if !note.detail.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(ellipsize(&note.detail, 60)).size(10.5).color(MUTED));
                        }
                        if notes.len() > 1 {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!("+{} more", notes.len() - 1))
                                    .size(10.0)
                                    .color(MUTED),
                            )
                            .on_hover_text(
                                notes
                                    .iter()
                                    .skip(1)
                                    .map(|note| note.title.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("v{APP_VERSION}")).size(10.0).color(MUTED));
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Developed with ♥ from gamers for gamers")
                                .size(10.0)
                                .color(MUTED),
                        );
                    });
                });
            });
        if open_downloads {
            self.page = Page::Downloads;
        }
    }

    /// Live status for the bottom bar, most-important-first: environment problems (Steam missing,
    /// Service problems, conflicting/tampering software), then the latest activity message, then the
    /// benign "Service ready" state. The status bar shows the first; the rest fold into a "+N more".
    pub fn collect_notifications(&self) -> Vec<Notification> {
        let mut errors: Vec<Notification> = Vec::new();
        let mut activity: Vec<Notification> = Vec::new();
        let mut benign: Vec<Notification> = Vec::new();

        if self.steam.root.is_none() {
            errors.push(Notification::error(
                "Steam not found",
                "Set the Steam folder in Settings.",
            ));
        }

        match self.service_status.as_ref().map(|status| status.state) {
            Some(SteamServiceState::NotInstalled) => errors.push(Notification::warn(
                "Steam Service not installed",
                "Install it in Settings to enable Add to Steam.",
            )),
            Some(SteamServiceState::UpdateAvailable) => errors.push(Notification::warn(
                "Steam Service out of date",
                "Reinstall it in Settings to update.",
            )),
            Some(SteamServiceState::Error) => errors.push(Notification::error(
                "Steam Service problem",
                self.service_status
                    .as_ref()
                    .map_or("", |status| status.message.as_str()),
            )),
            Some(SteamServiceState::Current) => {
                benign.push(Notification::ok("Steam Service ready", ""));
            }
            None if self.service_receiver.is_some() => {
                benign.push(Notification::info("Checking Steam Service…", ""));
            }
            None => {}
        }

        for name in &self.conflicts.names {
            let detail = if name == "Modified Steam files" {
                "Foreign backup files were found in the Steam folder. They can break the Steam Service."
            } else {
                "Manages the same Steam files and can break activation. Remove it, then restart Steam."
            };
            errors.push(Notification::error(name, detail));
        }

        let status = self.status.trim();
        if !status.is_empty() {
            activity.push(if self.status_error {
                Notification::error(status, "")
            } else {
                Notification::info(status, "")
            });
        }

        errors.extend(activity);
        errors.extend(benign);
        errors
    }
}

