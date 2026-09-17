use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;

pub fn sidebar_link(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    active: bool,
    badge: Option<(&str, Color32)>,
) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 40.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());

    let fill = if active {
        Color32::from_rgba_unmultiplied(0, 225, 250, 26)
    } else {
        lerp_color(
            Color32::TRANSPARENT,
            Color32::from_rgba_unmultiplied(255, 255, 255, 12),
            hover,
        )
    };
    let stroke = if active {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 225, 250, 85))
    } else {
        Stroke::new(
            1.0,
            lerp_color(
                Color32::TRANSPARENT,
                Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                hover,
            ),
        )
    };

    ui.painter()
        .rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);

    // Left active indicator pill
    if active {
        let indicator_h = 20.0;
        let indicator = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + 2.0, rect.center().y - indicator_h / 2.0),
            Vec2::new(3.5, indicator_h),
        );
        ui.painter().rect_filled(indicator, 2.0, ACCENT);
    }

    // Lucide Icon
    let icon_color = if active {
        ACCENT
    } else {
        lerp_color(MUTED, TEXT, hover * 0.7)
    };
    let icon_font = FontId::proportional(16.5);
    let icon_galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), icon_font, icon_color);
    let icon_pos = egui::pos2(rect.left() + 14.0, rect.center().y - icon_galley.size().y / 2.0);
    ui.painter().galley(icon_pos, icon_galley, icon_color);

    // Label
    let text_color = if active {
        Color32::WHITE
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    let text_font = if active {
        FontId::proportional(14.5)
    } else {
        FontId::proportional(14.0)
    };
    let text_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), text_font, text_color);
    let text_pos = egui::pos2(rect.left() + 42.0, rect.center().y - text_galley.size().y / 2.0);
    ui.painter().galley(text_pos, text_galley, text_color);

    // Optional Badge on the right
    if let Some((badge_label, badge_color)) = badge {
        let b_font = FontId::proportional(11.0);
        let b_galley = ui
            .painter()
            .layout_no_wrap(badge_label.to_string(), b_font, Color32::WHITE);
        let b_pad = Vec2::new(6.0, 2.0);
        let b_size = b_galley.size() + 2.0 * b_pad;
        let b_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 14.0 - b_size.x / 2.0, rect.center().y),
            b_size,
        );
        ui.painter().rect_filled(b_rect, 4.0, badge_color);
        let bp = egui::pos2(
            b_rect.left() + b_pad.x,
            b_rect.center().y - b_galley.size().y / 2.0,
        );
        ui.painter().galley(bp, b_galley, Color32::WHITE);
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn sidebar_section_label(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new(title)
                .size(11.5)
                .strong()
                .color(Color32::from_rgb(85, 115, 145)),
        );
    });
    ui.add_space(6.0);
}

impl DrydockApp {
    pub fn sidebar_nav(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("sidebar_nav")
            .exact_size(SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(14, 16))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(root, |ui| {
                // Header (Pinned at Top): App Icon, Brand & Version
                ui.horizontal(|ui| {
                    let icon_size = 32.0;
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(icon_size), Sense::hover());
                    ui.painter().rect_filled(rect, 8.0, Color32::from_rgb(12, 28, 44));
                    ui.painter().rect_stroke(
                        rect,
                        8.0,
                        Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.5)),
                        egui::StrokeKind::Inside,
                    );
                    egui::Image::new(egui::include_image!("../../../../../assets/app-icon.png"))
                        .corner_radius(7)
                        .paint_at(ui, rect.shrink(2.0));

                    ui.add_space(8.0);

                    ui.label(RichText::new("Drydock").size(18.0).strong().color(Color32::WHITE));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                            .stroke(Stroke::new(
                                1.0,
                                Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                            ))
                            .corner_radius(5)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("v{APP_VERSION}"))
                                        .size(11.5)
                                        .strong()
                                        .color(MUTED),
                                );
                            });
                    });
                });

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(14.0);

                // Bottom pinned status info card
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(4.0);
                    let steam_ok = self.steam.root.is_some();
                    let (dot_color, status_text) = if steam_ok {
                        (VERDIGRIS, "Steam Connected")
                    } else {
                        (DANGER, "Steam Offline")
                    };

                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(12, 22, 34, 180))
                        .stroke(Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                        ))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                                ui.painter().circle_filled(dot.center(), 3.5, dot_color);
                                ui.add_space(4.0);
                                ui.label(RichText::new(status_text).size(12.5).color(TEXT));
                            });
                        });
                    ui.add_space(10.0);

                    // Scrollable Navigation Area in the middle
                    egui::ScrollArea::vertical()
                        .id_salt("sidebar_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                // STOREFRONT section
                                sidebar_section_label(ui, "STOREFRONT");
                                let dl_badge = if self.download_running() {
                                    Some(("ACTIVE", ACCENT))
                                } else if self.download_paused {
                                    Some(("PAUSED", DANGER))
                                } else if !self.settings.download_queue.is_empty() {
                                    Some(("QUEUED", AMBER))
                                } else {
                                    None
                                };
                                for (page, icon, label, badge) in [
                                    (Page::Home, icons::STORE, "Store", None),
                                    (Page::Library, icons::LIBRARY, "Library", None),
                                    (Page::Downloads, icons::DOWNLOAD, "Downloads", dl_badge),
                                ] {
                                    let is_active = self.page == page
                                        || (page == Page::Home && self.page == Page::SeeAll);
                                    if sidebar_link(ui, icon, label, is_active, badge).clicked() {
                                        self.page = page;
                                    }
                                    ui.add_space(3.0);
                                }

                                ui.add_space(16.0);

                                // SERVICES & TOOLS section
                                sidebar_section_label(ui, "SERVICES & TOOLS");
                                for (page, icon, label) in [
                                    (Page::Activation, icons::ACTIVATION, "Activation"),
                                    (Page::Tools, icons::TOOLS, "Tools"),
                                    (Page::Cloud, icons::CLOUD, "Cloud"),
                                ] {
                                    if sidebar_link(ui, icon, label, self.page == page, None).clicked() {
                                        self.page = page;
                                    }
                                    ui.add_space(3.0);
                                }

                                ui.add_space(16.0);

                                // SYSTEM section
                                sidebar_section_label(ui, "SYSTEM");
                                let updates_badge = if self.update_receiver.is_some() {
                                    Some(("...", ACCENT))
                                } else {
                                    None
                                };
                                for (page, icon, label, badge) in [
                                    (Page::Settings, icons::SETTINGS, "Settings", None),
                                    (Page::Updates, icons::UPDATES, "Updates", updates_badge),
                                    (Page::Guide, icons::HELP, "Help & Guide", None),
                                ] {
                                    if sidebar_link(ui, icon, label, self.page == page, badge).clicked() {
                                        self.page = page;
                                    }
                                    ui.add_space(3.0);
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
    pub fn status_bar(&mut self, root: &mut egui::Ui) {
        let (download_label, download_accent) = self.download_status_label();
        let is_download_active = self.download_running();
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
                let bar_rect = ui.max_rect();

                // Centered Downloads button pill (Steam-style)
                let dl_text = format!("{}  {}", icons::DOWNLOAD, download_label);
                let font = FontId::proportional(14.0);
                let galley = ui.painter().layout_no_wrap(dl_text, font, TEXT);
                let padding = Vec2::new(14.0, 5.0);
                let desired_size = galley.size() + 2.0 * padding;
                let dl_rect = egui::Rect::from_center_size(bar_rect.center(), desired_size);

                let dl_resp = ui.interact(dl_rect, ui.id().with("dl_center_pill"), Sense::click());
                if dl_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let fill = if dl_resp.hovered() {
                    SURFACE_RAISED
                } else {
                    SURFACE
                };
                let stroke = if dl_resp.hovered() {
                    Stroke::new(1.0, ACCENT)
                } else if is_download_active {
                    Stroke::new(1.0, download_accent)
                } else {
                    Stroke::new(1.0, BORDER)
                };
                ui.painter()
                    .rect(dl_rect, 8.0, fill, stroke, egui::StrokeKind::Inside);
                let text_pos = egui::pos2(
                    dl_rect.left() + padding.x,
                    dl_rect.center().y - galley.size().y / 2.0,
                );
                let text_color = if dl_resp.hovered() {
                    Color32::WHITE
                } else if is_download_active {
                    download_accent
                } else {
                    TEXT
                };
                ui.painter().galley(text_pos, galley, text_color);
                if dl_resp.clicked() {
                    open_downloads = true;
                }
                dl_resp.on_hover_text("View Downloads");

                ui.horizontal_centered(|ui| {
                    // Left side: Notifications summary pill
                    let notes = self.collect_notifications();
                    if let Some(note) = notes.first() {
                        let has_more = notes.len() > 1;
                        let title = ellipsize(&note.title, 45);
                        let more_text = if has_more {
                            format!("  +{} more", notes.len() - 1)
                        } else {
                            String::new()
                        };

                        let frame_fill = if self.notifications_open {
                            SURFACE_RAISED
                        } else {
                            SURFACE
                        };
                        let frame_stroke = if self.notifications_open {
                            Stroke::new(1.0, ACCENT)
                        } else {
                            Stroke::new(1.0, BORDER)
                        };

                        let pill_frame = egui::Frame::new()
                            .fill(frame_fill)
                            .stroke(frame_stroke)
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(12, 5))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                                    ui.painter().circle_filled(dot.center(), 4.0, note.accent);
                                    ui.add_space(4.0);
                                    ui.label(RichText::new(&title).size(14.0).color(TEXT));
                                    if has_more {
                                        ui.label(RichText::new(&more_text).size(14.0).color(MUTED));
                                    }
                                });
                            });

                        let pill_resp = ui.interact(
                            pill_frame.response.rect,
                            ui.id().with("notif_pill_click"),
                            Sense::click(),
                        );
                        if pill_resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if pill_resp.clicked() {
                            self.notifications_open = !self.notifications_open;
                        }
                        let tooltip = if notes.len() > 1 {
                            format!("Click to view all {} notifications", notes.len())
                        } else {
                            "Click to view notifications".to_string()
                        };
                        pill_resp.on_hover_text(tooltip);
                    }

                    // Right side items
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("v{APP_VERSION}")).size(14.0).color(MUTED));
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("Developed with ♥ for gamers")
                                .size(14.0)
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
