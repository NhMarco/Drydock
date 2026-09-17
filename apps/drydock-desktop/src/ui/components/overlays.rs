
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;




impl DrydockApp {
    pub fn crack_removal_window(&mut self, context: &egui::Context) {
        let Some(pending) = &self.pending_crack else {
            return;
        };
        let count = pending.files.len();
        let names: Vec<String> = pending
            .files
            .iter()
            .take(14)
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect();
        let mut remove = false;
        let mut cancel = false;
        egui::Window::new("Crack / hypervisor files found")
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                ui.label(
                    RichText::new(
                        "Hypervisor- or crackfiles were found, the files need to be removed to activate it.",
                    )
                    .strong()
                    .color(DANGER),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{count} file(s)/folder(s) will be deleted from the game folder:"
                    ))
                    .size(14.0)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    for name in &names {
                        ui.label(RichText::new(format!("•  {name}")).size(14.0).color(TEXT));
                    }
                    if count > names.len() {
                        ui.label(
                            RichText::new(format!("…and {} more", count - names.len()))
                                .size(14.0)
                                .color(MUTED),
                        );
                    }
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(primary_button("REMOVE"))
                        .on_hover_text("Delete these files, then continue")
                        .clicked()
                    {
                        remove = true;
                    }
                    if ui.add(ghost_button("CANCEL")).clicked() {
                        cancel = true;
                    }
                });
            });
        if remove {
            self.start_crack_removal();
        } else if cancel {
            self.pending_crack = None;
            self.status = "Activation cancelled — crack files were not removed.".into();
            self.status_error = false;
        }
    }

    pub fn entitlement_success_window(&mut self, context: &egui::Context) {
        let Some(app_name) = self.entitlement_success_app.clone() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new(format!("{app_name} entitlement verified!"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(context, |ui| {
                ui.label(
                    RichText::new("The signed activation response is valid for this game and this device.")
                        .strong()
                        .color(ACCENT_SOFT),
                );
                ui.add_space(12.0);
                ui.label(RichText::new("We hope you have a great time with your game!").color(TEXT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Please take a moment to leave a review and share your experience with the community in Discord.",
                    )
                    .color(MUTED),
                );
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.add(primary_button("CLOSE")).on_hover_text("Close this dialog").clicked();
                });
            });
        if !open || close {
            self.entitlement_success_app = None;
        }
    }

    pub fn busy_overlay(&self, context: &egui::Context) {
        let Some(label) = &self.busy_label else {
            return;
        };
        let screen = context.content_rect();
        egui::Area::new("busy_overlay".into())
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(4, 5, 13, 220));
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(12.0);
                            ui.label(RichText::new(label).size(15.0).strong().color(TEXT));
                        });
                    });
                });
            });
    }

    pub fn notifications_window(&mut self, context: &egui::Context) {
        if !self.notifications_open {
            return;
        }
        let notes = self.collect_notifications();
        let mut close = false;
        let mut open_page: Option<Page> = None;

        egui::Window::new("Notifications")
            .open(&mut self.notifications_open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(egui::Align2::LEFT_BOTTOM, [240.0, -48.0])
            .anchor(egui::Align2::LEFT_BOTTOM, [SIDEBAR_WIDTH + 8.0, -48.0])
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR_FILL)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(12)
                    .inner_margin(16),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("NOTIFICATIONS")
                            .size(15.0)
                            .strong()
                            .color(ACCENT_SOFT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let count_text = format!("{} active", notes.len());
                        ui.label(RichText::new(count_text).size(14.0).color(MUTED));
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                if notes.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("No active notifications.")
                                .size(14.0)
                                .color(MUTED),
                        );
                        ui.add_space(14.0);
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            for (i, note) in notes.iter().enumerate() {
                                if i > 0 {
                                    ui.add_space(8.0);
                                }
                                egui::Frame::new()
                                    .fill(SURFACE)
                                    .stroke(Stroke::new(1.0, BORDER))
                                    .corner_radius(8)
                                    .inner_margin(egui::Margin::symmetric(12, 10))
                                    .show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                                                ui.painter().circle_filled(dot.center(), 4.0, note.accent);
                                                ui.add_space(4.0);
                                                ui.label(RichText::new(&note.title).size(14.0).strong().color(TEXT));
                                            });
                                            if !note.detail.is_empty() {
                                                ui.add_space(4.0);
                                                ui.label(
                                                    RichText::new(&note.detail)
                                                        .size(14.0)
                                                        .color(MUTED),
                                                );
                                            }

                                            if note.title.contains("Steam Service") || note.title.contains("Steam not found") {
                                                ui.add_space(6.0);
                                                if ui.add(ghost_button("OPEN SETTINGS").min_size(Vec2::new(120.0, 26.0))).clicked() {
                                                    open_page = Some(Page::Settings);
                                                }
                                            } else if note.title.contains("Download") {
                                                ui.add_space(6.0);
                                                if ui.add(ghost_button("OPEN DOWNLOADS").min_size(Vec2::new(120.0, 26.0))).clicked() {
                                                    open_page = Some(Page::Downloads);
                                                }
                                            }
                                        });
                                    });
                            }
                        });
                }

                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(ghost_button("DISMISS")).clicked() {
                        close = true;
                    }
                });
            });

        if close {
            self.notifications_open = false;
        }
        if let Some(page) = open_page {
            self.page = page;
            self.notifications_open = false;
        }
    }
}

