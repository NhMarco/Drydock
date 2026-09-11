
use drydock_core::*;
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;


pub fn steam_service_card(
    ui: &mut egui::Ui,
    steam: &SteamDiscovery,
    status: Option<&SteamServiceStatus>,
    service_busy: bool,
    restart_enabled: bool,
) -> SteamServiceCardAction {
    let mut action = SteamServiceCardAction::None;
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(14)
        .show(ui, |ui| {
            section_label(ui, "STEAM SERVICE");
            let Some(root) = &steam.root else {
                ui.label(RichText::new("STEAM NOT FOUND").size(15.0).strong().color(DANGER));
                ui.label(
                    RichText::new("Set the Steam folder in Settings.")
                        .size(8.5)
                        .color(MUTED),
                );
                return;
            };

            let (state_label, color, message, installed) = match status {
                Some(status) => {
                    let (label, color) = match status.state {
                        SteamServiceState::Current => ("CURRENT", ACCENT_SOFT),
                        SteamServiceState::UpdateAvailable => ("UPDATE AVAILABLE", ACCENT),
                        SteamServiceState::NotInstalled => ("NOT INSTALLED", MUTED),
                        SteamServiceState::Error => ("ATTENTION", DANGER),
                    };
                    let installed = status.state != SteamServiceState::NotInstalled;
                    (label, color, status.message.clone(), installed)
                }
                None if service_busy => ("CHECKING…", MUTED, String::new(), false),
                None => ("UNKNOWN", MUTED, String::new(), false),
            };
            ui.add_space(6.0);
            status_pill(ui, state_label, color);
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(RichText::new(root.display().to_string()).size(8.5).color(MUTED)).truncate(),
            );
            if !message.is_empty() {
                ui.add(egui::Label::new(RichText::new(message).size(9.0).color(MUTED)).wrap());
            }
            ui.add_space(10.0);

            let full = ui.available_width();
            let is_current = matches!(
                status.map(|status| status.state),
                Some(SteamServiceState::Current)
            );
            // The primary button carries the state action (Install/Update/Repair). When the
            // service is already current there is nothing to install, so it is hidden and the
            // Reinstall/Uninstall pair below takes over.
            if !is_current {
                let primary_label = status
                    .map_or("INSTALL", SteamServiceStatus::action_text)
                    .to_uppercase();
                if ui
                    .add_enabled(
                        !service_busy,
                        primary_button(&primary_label).min_size(Vec2::new(full, 34.0)),
                    )
                    .clicked()
                {
                    action = SteamServiceCardAction::Install;
                }
                if installed {
                    ui.add_space(6.0);
                }
            }
            // Reinstall and uninstall only make sense once the service is installed. They are
            // stacked full-width so their labels never overflow the narrow sidebar card.
            if installed {
                if ui
                    .add_enabled(
                        !service_busy,
                        ghost_button("REINSTALL").min_size(Vec2::new(full, 30.0)),
                    )
                    .on_hover_text("Download and reinstall the Steam Service files")
                    .clicked()
                {
                    action = SteamServiceCardAction::Reinstall;
                }
                ui.add_space(6.0);
                if ui
                    .add_enabled(
                        !service_busy,
                        ghost_button("UNINSTALL").min_size(Vec2::new(full, 30.0)),
                    )
                    .on_hover_text("Remove the Steam Service files and restart Steam")
                    .clicked()
                {
                    action = SteamServiceCardAction::Uninstall;
                }
            }
            ui.add_space(6.0);
            if ui
                .add_enabled(
                    restart_enabled && !service_busy,
                    ghost_button("RESTART STEAM").min_size(Vec2::new(full, 30.0)),
                )
                .on_hover_text("Stop and restart the Steam client")
                .clicked()
            {
                action = SteamServiceCardAction::Restart;
            }
        });
    action
}

/// A segmented-control tab for switching How It Works walkthroughs.

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
                    .size(10.5)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    for name in &names {
                        ui.label(RichText::new(format!("•  {name}")).size(10.5).color(TEXT));
                    }
                    if count > names.len() {
                        ui.label(
                            RichText::new(format!("…and {} more", count - names.len()))
                                .size(10.0)
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
                            ui.label(RichText::new(label).size(13.0).strong().color(TEXT));
                        });
                    });
                });
            });
    }
}

