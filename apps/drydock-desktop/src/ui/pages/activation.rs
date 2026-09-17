use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke, Vec2};

use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

fn step_pill(ui: &mut egui::Ui, step: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(6), ACCENT_DEEP);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(6),
        Stroke::new(1.0, ACCENT),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        step,
        FontId::monospace(12.5),
        Color32::WHITE,
    );
}

pub fn activation_segment(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(110.0, 36.0), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        ACCENT
    } else {
        lerp_color(SURFACE, SURFACE_RAISED, hover)
    };
    let stroke = if active {
        Stroke::NONE
    } else {
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover))
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.5),
        if active {
            Color32::from_rgb(4, 14, 24)
        } else {
            TEXT
        },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// The EA provider body — a placeholder until EA activation ships.
pub fn activation_ea_body(ui: &mut egui::Ui) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(16)
        .inner_margin(36)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(icons::SHIELD).size(40.0).color(AMBER));
                ui.add_space(14.0);
                ui.label(
                    RichText::new("EA Activation Coming Soon")
                        .size(20.0)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("EA Desktop / Origin entitlement support is currently in development.")
                        .size(14.0)
                        .color(MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Please use Steam or Ubisoft activation for supported games.")
                        .size(13.5)
                        .color(MUTED),
                );
                ui.add_space(8.0);
            });
        });
}

pub fn response_code_fields(ui: &mut egui::Ui, characters: &mut [String; 8]) {
    let pasted = ui.input(|input| {
        input.events.iter().rev().find_map(|event| match event {
            egui::Event::Paste(value) => Some(value.clone()),
            _ => None,
        })
    });
    let focused = (0..characters.len()).find(|index| {
        ui.memory(|memory| memory.has_focus(egui::Id::new(("response_code_character", *index))))
    });
    if let (Some(value), Some(index)) = (pasted, focused) {
        let focus = distribute_response_code(characters, index, &value);
        ui.memory_mut(|memory| {
            memory.request_focus(egui::Id::new(("response_code_character", focus)));
        });
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for index in 0..characters.len() {
            let id = egui::Id::new(("response_code_character", index));
            let was_empty = characters[index].is_empty();
            let is_focused = ui.memory(|m| m.has_focus(id));
            let has_val = !characters[index].is_empty();

            let (bg, stroke) = if is_focused {
                (SURFACE_RAISED, Stroke::new(1.5, ACCENT))
            } else if has_val {
                (SURFACE_RAISED, Stroke::new(1.0, ACCENT_SOFT))
            } else {
                (Color32::from_rgb(10, 18, 28), Stroke::new(1.0, BORDER))
            };

            let response = egui::Frame::new()
                .fill(bg)
                .stroke(stroke)
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(0, 4))
                .show(ui, |ui| {
                    ui.set_height(46.0);
                    ui.set_width(40.0);
                    let te = egui::TextEdit::singleline(&mut characters[index])
                        .id(id)
                        .char_limit(1)
                        .frame(egui::Frame::NONE)
                        .font(FontId::monospace(20.0))
                        .text_color(if has_val { ACCENT } else { TEXT })
                        .horizontal_align(egui::Align::Center);
                    ui.add(te)
                })
                .inner;

            if response.changed() {
                characters[index] = normalize_response_fragment(&characters[index])
                    .chars()
                    .next()
                    .map_or_else(String::new, |character| character.to_string());
                if !characters[index].is_empty() && index + 1 < characters.len() {
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(("response_code_character", index + 1)));
                    });
                }
            }
            if response.has_focus() {
                let move_left = ui.input(|input| input.key_pressed(egui::Key::ArrowLeft));
                let move_right = ui.input(|input| input.key_pressed(egui::Key::ArrowRight));
                let back_to_previous = was_empty && ui.input(|input| input.key_pressed(egui::Key::Backspace));
                if (move_left || back_to_previous) && index > 0 {
                    if back_to_previous {
                        characters[index - 1].clear();
                    }
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(("response_code_character", index - 1)));
                    });
                } else if move_right && index + 1 < characters.len() {
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(("response_code_character", index + 1)));
                    });
                }
            }
        }
    });
}

impl DrydockApp {
    pub fn poll_activation_request(&mut self) {
        let Some(receiver) = self.activation_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.activation_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(code) => {
                        self.status = if is_short_activation_code(&code) {
                            "Short activation request created".into()
                        } else {
                            "The short-code service is unavailable. A complete fallback request was created."
                                .into()
                        };
                        self.status_error = false;
                        self.activation_request_code = code;
                    }
                    Err(error) => {
                        self.status = format!("Activation request failed: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.activation_receiver = None;
                self.busy_label = None;
                self.status = "The activation request ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    pub fn go_to_activation(&mut self, app_id: u32) {
        self.select_activation_game(app_id);
        self.page = Page::Activation;
    }

    /// Selects a game for activation: fills the search field with its name (collapsing the result
    /// list), prefills the folder from Steam when installed, and resets any in-flight code state.
    pub fn select_activation_game(&mut self, app_id: u32) {
        let name = self
            .catalog
            .iter()
            .find(|entry| entry.app_id == app_id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| format!("APP {app_id}"));
        self.selected_app = Some(app_id);
        self.activation_search = name;
        self.activation_request_code.clear();
        self.activation_root = None;
        self.verified_entitlement = None;
        self.entitlement_success_app = None;
        self.response_code = std::array::from_fn(|_| String::new());
        self.activation_path = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(|manifest| manifest.install_dir().display().to_string())
            .unwrap_or_default();
    }

    /// Loads in-game language options from an arbitrary folder (searching its subfolders for the
    /// Steam-settings language files), for the Tools tab language changer.
    pub fn activation_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  Activation", icons::ACTIVATION));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.activation_switcher(ui);
            });
        });
        ui.add_space(18.0);

        match self.activation_provider {
            ActivationProvider::Steam => self.activation_steam_body(ui),
            ActivationProvider::Ubisoft => self.activation_ubisoft_body(ui),
            ActivationProvider::Ea => activation_ea_body(ui),
        }
    }

    /// The STEAM / UBISOFT / EA segmented switcher at the top of the Activation page.
    pub fn activation_switcher(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (provider, label) in [
                (ActivationProvider::Steam, "STEAM"),
                (ActivationProvider::Ubisoft, "UBISOFT"),
                (ActivationProvider::Ea, "EA"),
            ] {
                if activation_segment(ui, label, self.activation_provider == provider) {
                    self.activation_provider = provider;
                }
            }
        });
    }

    /// The Steam activation flow: pick a game and folder, generate a machine-bound
    /// request code, then paste the bot's response code to verify and install the entitlement.
    pub fn activation_steam_body(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    step_pill(ui, "1");
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("SELECT GAME TO ACTIVATE")
                            .size(13.0)
                            .strong()
                            .color(ACCENT),
                    );
                });
                ui.add_space(10.0);

                let selector_width = ui.available_width();
                if let Some(app_id) = game_search_box(
                    ui,
                    "activation_search",
                    &mut self.activation_search,
                    self.selected_app,
                    &self.catalog,
                    &self.header_resolver,
                    selector_width,
                    false,
                    |_| true,
                ) {
                    self.select_activation_game(app_id);
                }

                if let Some(app_id) = self.selected_app {
                    ui.add_space(20.0);
                    ui.separator();
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        step_pill(ui, "2");
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("LOCATE GAME INSTALLATION FOLDER")
                                .size(13.0)
                                .strong()
                                .color(ACCENT),
                        );
                    });
                    ui.add_space(10.0);

                    let field_width = (ui.available_width() - 136.0).max(200.0);
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [field_width, 40.0],
                            egui::TextEdit::singleline(&mut self.activation_path)
                                .hint_text("…\\steamapps\\common\\Game  (or the repack folder)")
                                .margin(egui::Margin::symmetric(12, 10)),
                        );
                        if ui
                            .add_sized([126.0, 40.0], ghost_button(&format!("{}  BROWSE", icons::FOLDER)))
                            .clicked()
                            && let Some(folder) = rfd::FileDialog::new().pick_folder()
                        {
                            self.activation_path = folder.display().to_string();
                            self.activation_root = None;
                        }
                    });

                    ui.add_space(14.0);
                    let previous_verify = self.settings.verify_before_activation;
                    let mut verify_changed = false;
                    ui.horizontal(|ui| {
                        if toggle_switch(ui, &mut self.settings.verify_before_activation, ACCENT_SOFT).changed() {
                            verify_changed = true;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Verify the game files first")
                                .size(13.0)
                                .color(TEXT),
                        )
                        .on_hover_text(
                            "Checks every file against the game's depot manifests before the code is made, so \
                             only an intact game is activated. Needs the game's depot package.",
                        );
                    });
                    if verify_changed && let Err(error) = self.persist_settings() {
                        self.settings.verify_before_activation = previous_verify;
                        self.status = format!("The verification setting could not be saved: {error}");
                        self.status_error = true;
                    }
                    if self.activation_verify.is_some() {
                        let progress = self
                            .download_job
                            .as_ref()
                            .and_then(|job| job.progress.as_ref())
                            .filter(|progress| progress.total_bytes > 0)
                            .map_or(0, |progress| {
                                progress.done_bytes.saturating_mul(100) / progress.total_bytes
                            });
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("Verifying the game files… {progress}%"))
                                .size(11.5)
                                .color(MUTED),
                        );
                    }

                    ui.add_space(16.0);
                    let busy = self.activation_check_receiver.is_some()
                        || self.activation_remove_receiver.is_some()
                        || self.activation_receiver.is_some()
                        || self.activation_verify.is_some();
                    let can_generate = !self.activation_path.trim().is_empty() && !busy;

                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                can_generate,
                                primary_button(&format!("{}  GENERATE ACTIVATION CODE", icons::SPARKLES))
                                    .min_size(Vec2::new(240.0, 40.0)),
                            )
                            .on_hover_text("Verify the game folder, then create a machine-bound request code")
                            .clicked()
                        {
                            self.start_activation_check(app_id, PathBuf::from(self.activation_path.trim()));
                        }
                        if busy {
                            ui.add_space(8.0);
                            ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                        }
                    });
                } else if self.catalog.is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Loading game catalog…").size(13.5).color(MUTED));
                }
            });

        if !self.activation_request_code.is_empty() {
            let short_request = is_short_activation_code(&self.activation_request_code);
            ui.add_space(16.0);
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(16)
                .inner_margin(24)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        step_pill(ui, "3");
                        ui.add_space(8.0);
                        ui.label(RichText::new("SUBMIT REQUEST & ENTER RESPONSE").size(13.0).strong().color(ACCENT));
                    });
                    ui.add_space(14.0);

                    egui::Frame::new()
                        .fill(Color32::from_rgb(12, 18, 26))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(28, 42, 60)))
                        .corner_radius(12)
                        .inner_margin(16)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(if short_request {
                                        "YOUR ACTIVATION REQUEST CODE"
                                    } else {
                                        "COMPLETE FALLBACK REQUEST"
                                    })
                                    .size(11.5)
                                    .strong()
                                    .color(MUTED),
                                );

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui
                                        .add(ghost_button("COPY CODE").compact())
                                        .on_hover_text("Copy activation request code to clipboard")
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(self.activation_request_code.clone());
                                        self.status = "Activation code copied to clipboard".into();
                                        self.status_error = false;
                                    }
                                });
                            });
                            ui.add_space(8.0);

                            if short_request {
                                ui.label(
                                    RichText::new(&self.activation_request_code)
                                        .size(24.0)
                                        .monospace()
                                        .strong()
                                        .color(TEXT),
                                );
                            } else {
                                ui.add_sized(
                                    [ui.available_width(), 78.0],
                                    egui::TextEdit::multiline(&mut self.activation_request_code)
                                        .font(egui::TextStyle::Monospace)
                                        .margin(egui::Margin::symmetric(12, 10))
                                        .interactive(false),
                                );
                            }
                        });

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(if short_request {
                            "Send this short request code in your activation ticket. It expires after 30 minutes."
                        } else {
                            "The upload service could not be reached. Copy this complete fallback request into your ticket."
                        })
                        .size(13.0)
                        .color(ACCENT_SOFT),
                    );

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(16.0);

                    ui.label(RichText::new("ENTER 8-DIGIT RESPONSE CODE FROM BOT").size(12.5).strong().color(TEXT));
                    ui.add_space(8.0);
                    response_code_fields(ui, &mut self.response_code);

                    ui.add_space(16.0);
                    let response_is_complete = self.response_code.iter().all(|part| {
                        part.len() == 1 && part.chars().all(|value| value.is_ascii_alphanumeric())
                    });
                    let is_verifying = self.activation_verify_receiver.is_some();

                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                response_is_complete && !is_verifying,
                                primary_button(&format!("{}  ACTIVATE GAME", icons::CHECK))
                                    .min_size(Vec2::new(180.0, 40.0)),
                            )
                            .on_hover_text("Verify and install signed activation entitlement")
                            .clicked()
                            && let Some(app_id) = self.selected_app
                        {
                            self.verify_activation_response(app_id);
                        }
                        if is_verifying {
                            ui.add_space(8.0);
                            ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                            ui.label(RichText::new("Verifying entitlement…").size(13.0).color(MUTED));
                        }
                    });

                    if let Some(entitlement) = &self.verified_entitlement {
                        ui.add_space(16.0);
                        egui::Frame::new()
                            .fill(Color32::from_rgb(14, 38, 30))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(34, 197, 94)))
                            .corner_radius(8)
                            .inner_margin(12)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(icons::CHECK).size(16.0).color(Color32::from_rgb(34, 197, 94)));
                                    ui.label(
                                        RichText::new(format!(
                                            "SIGNED ENTITLEMENT VERIFIED · {} protected bytes · Valid until {}",
                                            entitlement.payload_bytes, entitlement.expires_utc
                                        ))
                                        .size(13.0)
                                        .strong()
                                        .color(Color32::from_rgb(74, 222, 128)),
                                    );
                                });
                            });
                    }
                });
        }
    }

    /// The Ubisoft activation flow: pick a game + folder, PREPARE (adds magicfiles, launches the
    /// game once, captures token_req.txt into a machine-bound activation code), then paste the bot's
    /// response code to install token.ini next to the game exe.
    pub fn activation_ubisoft_body(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    step_pill(ui, "1");
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("SELECT GAME TO ACTIVATE")
                            .size(13.0)
                            .strong()
                            .color(ACCENT),
                    );
                });
                ui.add_space(10.0);

                let selector_width = ui.available_width();
                if let Some(app_id) = game_search_box(
                    ui,
                    "ubisoft_search",
                    &mut self.activation_search,
                    self.selected_app,
                    &self.catalog,
                    &self.header_resolver,
                    selector_width,
                    false,
                    |_| true,
                ) {
                    self.select_activation_game(app_id);
                }

                if let Some(app_id) = self.selected_app {
                    ui.add_space(20.0);
                    ui.separator();
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        step_pill(ui, "2");
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("LOCATE GAME FOLDER & PREPARE")
                                .size(13.0)
                                .strong()
                                .color(ACCENT),
                        );
                    });
                    ui.add_space(10.0);

                    let field_width = (ui.available_width() - 136.0).max(200.0);
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [field_width, 40.0],
                            egui::TextEdit::singleline(&mut self.activation_path)
                                .hint_text("…\\steamapps\\common\\Game")
                                .margin(egui::Margin::symmetric(12, 10)),
                        );
                        if ui
                            .add_sized([126.0, 40.0], ghost_button(&format!("{}  BROWSE", icons::FOLDER)))
                            .clicked()
                            && let Some(folder) = rfd::FileDialog::new().pick_folder()
                        {
                            self.activation_path = folder.display().to_string();
                            self.ubisoft_activation_code.clear();
                        }
                    });

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(
                            "Drydock installs the Ubisoft magicfiles, launches the game once, and captures \
                             the generated token_req.txt. Close the game once the token request appears.",
                        )
                        .size(13.0)
                        .color(MUTED),
                    );

                    ui.add_space(14.0);
                    let busy = self.ubisoft_prepare_receiver.is_some();
                    let can_prepare = !self.activation_path.trim().is_empty() && !busy;
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                can_prepare,
                                primary_button(&format!("{}  PREPARE & LAUNCH GAME", icons::PLAY))
                                    .min_size(Vec2::new(240.0, 40.0)),
                            )
                            .on_hover_text(
                                "Install magicfiles, launch the game, and capture its token request",
                            )
                            .clicked()
                        {
                            self.start_ubisoft_prepare(app_id, PathBuf::from(self.activation_path.trim()));
                        }
                        if busy {
                            ui.add_space(8.0);
                            ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                        }
                    });
                } else if self.catalog.is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Loading game catalog…").size(13.5).color(MUTED));
                }
            });

        if !self.ubisoft_activation_code.is_empty() {
            ui.add_space(16.0);
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(16)
                .inner_margin(24)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        step_pill(ui, "3");
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("SUBMIT CODE & INSTALL TOKEN")
                                .size(13.0)
                                .strong()
                                .color(ACCENT),
                        );
                    });
                    ui.add_space(14.0);

                    egui::Frame::new()
                        .fill(Color32::from_rgb(12, 18, 26))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(28, 42, 60)))
                        .corner_radius(12)
                        .inner_margin(16)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("UBISOFT ACTIVATION CODE")
                                        .size(11.5)
                                        .strong()
                                        .color(MUTED),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui
                                        .add(ghost_button("COPY CODE").compact())
                                        .on_hover_text("Copy the Ubisoft activation code to clipboard")
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(self.ubisoft_activation_code.clone());
                                        self.status = "Activation code copied to clipboard".into();
                                        self.status_error = false;
                                    }
                                });
                            });
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(&self.ubisoft_activation_code)
                                    .size(24.0)
                                    .monospace()
                                    .strong()
                                    .color(TEXT),
                            );
                        });

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Send this code in your Ubisoft ticket. It expires after 30 minutes.")
                            .size(13.0)
                            .color(ACCENT_SOFT),
                    );

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(16.0);

                    ui.label(
                        RichText::new("ENTER 8-DIGIT RESPONSE CODE FROM BOT")
                            .size(12.5)
                            .strong()
                            .color(TEXT),
                    );
                    ui.add_space(8.0);
                    response_code_fields(ui, &mut self.response_code);

                    ui.add_space(16.0);
                    let response_is_complete = self.response_code.iter().all(|part| {
                        part.len() == 1 && part.chars().all(|value| value.is_ascii_alphanumeric())
                    });
                    let is_verifying = self.activation_verify_receiver.is_some();

                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                response_is_complete && !is_verifying,
                                primary_button(&format!("{}  ACTIVATE GAME", icons::CHECK))
                                    .min_size(Vec2::new(180.0, 40.0)),
                            )
                            .on_hover_text("Verify the response and install token.ini next to the game")
                            .clicked()
                            && let Some(app_id) = self.selected_app
                        {
                            self.verify_ubisoft_response(app_id);
                        }
                        if is_verifying {
                            ui.add_space(8.0);
                            ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                            ui.label(RichText::new("Verifying token…").size(13.0).color(MUTED));
                        }
                    });

                    if let Some(entitlement) = &self.verified_entitlement {
                        ui.add_space(16.0);
                        egui::Frame::new()
                            .fill(Color32::from_rgb(14, 38, 30))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(34, 197, 94)))
                            .corner_radius(8)
                            .inner_margin(12)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(icons::CHECK)
                                            .size(16.0)
                                            .color(Color32::from_rgb(34, 197, 94)),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "TOKEN INSTALLED · {} bytes · Valid until {}",
                                            entitlement.payload_bytes, entitlement.expires_utc
                                        ))
                                        .size(13.0)
                                        .strong()
                                        .color(Color32::from_rgb(74, 222, 128)),
                                    );
                                });
                            });
                    }
                });
        }
    }

    /// Runs the whole Ubisoft prepare sequence on a background thread (folder verify, magicfiles
    /// download + install, first launch, token_req.txt capture, activation-code generation).
    pub fn start_ubisoft_prepare(&mut self, app_id: u32, chosen: PathBuf) {
        if self.ubisoft_prepare_receiver.is_some() || self.activation_verify_receiver.is_some() {
            return;
        }
        self.ubisoft_activation_code.clear();
        self.ubisoft_exe_dir = None;
        self.verified_entitlement = None;
        self.response_code = std::array::from_fn(|_| String::new());
        self.status = "Preparing Ubisoft activation…".into();
        self.status_error = false;
        self.busy_label = Some("Adding magicfiles and launching the game…".into());
        let settings_directory = self.paths.settings_dir();
        let (sender, receiver) = mpsc::channel();
        self.ubisoft_prepare_receiver = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(prepare_ubisoft(app_id, &chosen, &settings_directory));
        });
    }

    pub fn poll_ubisoft_prepare(&mut self) {
        let Some(receiver) = self.ubisoft_prepare_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.ubisoft_prepare_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(prepared) => {
                        self.ubisoft_activation_code = prepared.activation_code;
                        self.ubisoft_exe_dir = Some(prepared.exe_dir);
                        self.status =
                            "Token request captured. Send the activation code in your Ubisoft ticket.".into();
                        self.status_error = false;
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.ubisoft_prepare_receiver = None,
        }
    }

    /// Downloads the bot's response token and installs its `token.ini` payload next to the game exe.
    /// Reuses the shared verify receiver/poll (the same signed-token install path as Steam).
    pub fn verify_ubisoft_response(&mut self, app_id: u32) {
        if self.activation_verify_receiver.is_some() {
            return;
        }
        let Some(target) = self.ubisoft_exe_dir.clone() else {
            self.status = "Prepare the Ubisoft activation first.".into();
            self.status_error = true;
            return;
        };
        let response_code = self.response_code.concat();
        self.verified_entitlement = None;
        self.busy_label = Some("Verifying and installing token.ini…".into());
        let settings_directory = self.paths.settings_dir();
        let (sender, receiver) = mpsc::channel();
        self.activation_verify_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = ActivationRequestService::new(settings_directory)
                .and_then(|service| service.download_and_install(&response_code, app_id, &target))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }
}
