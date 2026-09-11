use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;

#[allow(dead_code)]
pub fn cloud_text_row(ui: &mut egui::Ui, label: &str, value: &mut String, secret: bool) {
    ui.label(RichText::new(label).size(10.5).color(ACCENT));
    ui.add_sized(
        [ui.available_width(), 38.0],
        egui::TextEdit::singleline(value)
            .password(secret)
            .margin(egui::Margin::symmetric(12, 9)),
    );
    ui.add_space(8.0);
}

/// A labelled folder field with a Browse button that opens a native folder picker.
#[allow(dead_code)]
pub fn cloud_folder_row(ui: &mut egui::Ui, label: &str, hint: &str, value: &mut String) {
    ui.label(RichText::new(label).size(10.5).color(ACCENT));
    ui.horizontal(|ui| {
        ui.add_sized(
            [ui.available_width() - 96.0, 38.0],
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .margin(egui::Margin::symmetric(12, 9)),
        );
        if ui.add(ghost_button("BROWSE")).clicked() {
            let mut dialog = rfd::FileDialog::new().set_title(label);
            let current = std::path::Path::new(value.trim());
            if current.is_dir() {
                dialog = dialog.set_directory(current);
            }
            if let Some(folder) = dialog.pick_folder() {
                *value = folder.display().to_string();
            }
        }
    });
    ui.add_space(8.0);
}


pub fn activation_segment(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(108.0, 34.0), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        ACCENT_DEEP
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
        egui::CornerRadius::same(10),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        if active { Color32::WHITE } else { TEXT },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// The EA provider body — a placeholder until EA activation ships.
pub fn activation_ea_body(ui: &mut egui::Ui) {
    panel(ui, |ui| {
        section_label(ui, "EA ACTIVATION");
        ui.add_space(8.0);
        ui.label(
            RichText::new("EA activation is coming soon.")
                .size(13.0)
                .strong()
                .color(TEXT),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("This provider isn't available yet — use STEAM or UBISOFT for now.")
                .size(10.5)
                .color(MUTED),
        );
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
            let response = ui.add_sized(
                [42.0, 48.0],
                egui::TextEdit::singleline(&mut characters[index])
                    .id(id)
                    .char_limit(1)
                    .font(egui::TextStyle::Heading)
                    .horizontal_align(egui::Align::Center),
            );
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
        page_heading(ui, "ACTIVATION");
        ui.add_space(18.0);
        content_column(ui, CONTENT_WIDTH, |ui| {
            self.activation_switcher(ui);
            ui.add_space(16.0);
            match self.activation_provider {
                ActivationProvider::Steam => self.activation_steam_body(ui),
                ActivationProvider::Ubisoft => self.activation_ubisoft_body(ui),
                ActivationProvider::Ea => activation_ea_body(ui),
            }
        });
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

    /// The Steam activation flow (unchanged): pick a game and folder, generate a machine-bound
    /// request code, then paste the bot's response code to verify and install the entitlement.
    pub fn activation_steam_body(&mut self, ui: &mut egui::Ui) {
        panel(ui, |ui| {
            section_label(ui, "SELECT GAME");
            ui.add_space(8.0);
            let selector_width = ui.available_width();

            // Dynamic search: type a name or App ID, then pick from the fixed-height result list.
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
                ui.add_space(16.0);
                section_label(ui, "GAME FOLDER");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let field_width = (selector_width - 132.0).max(200.0);
                    ui.add_sized(
                        [field_width, 40.0],
                        egui::TextEdit::singleline(&mut self.activation_path)
                            .hint_text("…\\steamapps\\common\\Game  (or the repack folder)")
                            .margin(egui::Margin::symmetric(12, 10)),
                    );
                    if ui
                        .add_sized([120.0, 40.0], ghost_button("CHOOSE FOLDER"))
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        self.activation_path = folder.display().to_string();
                        self.activation_root = None;
                    }
                });

                ui.add_space(20.0);
                let busy = self.activation_check_receiver.is_some()
                    || self.activation_remove_receiver.is_some()
                    || self.activation_receiver.is_some();
                ui.horizontal(|ui| {
                    let can_generate = !self.activation_path.trim().is_empty() && !busy;
                    if ui
                        .add_enabled(can_generate, primary_button("GENERATE ACTIVATION CODE"))
                        .on_hover_text("Verify the game folder, then create a machine-bound request code")
                        .clicked()
                    {
                        self.start_activation_check(app_id, PathBuf::from(self.activation_path.trim()));
                    }
                });
            } else if self.catalog.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the game catalog…").size(10.5).color(MUTED));
            }
            if !self.activation_request_code.is_empty() {
                let short_request = is_short_activation_code(&self.activation_request_code);
                ui.add_space(18.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(18, 21, 24))
                    .corner_radius(12)
                    .inner_margin(18)
                    .show(ui, |ui| {
                        section_label(
                            ui,
                            if short_request {
                                "REQUEST CODE"
                            } else {
                                "COMPLETE FALLBACK REQUEST"
                            },
                        );
                        if short_request {
                            ui.label(
                                RichText::new(&self.activation_request_code)
                                    .size(22.0)
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
                        if ui
                            .add(ghost_button("COPY CODE"))
                            .on_hover_text("Copy the activation request code to the clipboard")
                            .clicked()
                        {
                            ui.ctx().copy_text(self.activation_request_code.clone());
                            self.status = "Activation code copied".into();
                            self.status_error = false;
                        }
                        ui.add_space(18.0);
                        section_label(ui, "RESPONSE CODE FROM THE BOT");
                        ui.add_space(7.0);
                        response_code_fields(ui, &mut self.response_code);
                        ui.add_space(12.0);
                        let response_is_complete = self.response_code.iter().all(|part| {
                            part.len() == 1 && part.chars().all(|value| value.is_ascii_alphanumeric())
                        });
                        if ui
                            .add_enabled(
                                response_is_complete && self.activation_verify_receiver.is_none(),
                                primary_button("ACTIVATE"),
                            )
                            .on_hover_text("Verify and install the activation response")
                            .clicked()
                            && let Some(app_id) = self.selected_app
                        {
                            self.verify_activation_response(app_id);
                        }
                        if let Some(entitlement) = &self.verified_entitlement {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "SIGNED ENTITLEMENT VERIFIED · {} protected bytes · valid until {}",
                                    entitlement.payload_bytes, entitlement.expires_utc
                                ))
                                .size(9.5)
                                .strong()
                                .color(ACCENT_SOFT),
                            );
                        }
                    });
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if short_request {
                        "Send the short request code in your Steam ticket. It expires after 30 minutes."
                    } else {
                        "The upload service could not be reached. Copy the complete fallback request into the ticket."
                    })
                    .size(10.5)
                    .color(ACCENT),
                );
            }
        });
    }

    /// The Ubisoft activation flow: pick a game + folder, PREPARE (adds magicfiles, launches the
    /// game once, captures token_req.txt into a machine-bound activation code), then paste the bot's
    /// response code to install token.ini next to the game exe.
    pub fn activation_ubisoft_body(&mut self, ui: &mut egui::Ui) {
        panel(ui, |ui| {
            section_label(ui, "SELECT GAME");
            ui.add_space(8.0);
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
                ui.add_space(16.0);
                section_label(ui, "GAME FOLDER");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let field_width = (selector_width - 132.0).max(200.0);
                    ui.add_sized(
                        [field_width, 40.0],
                        egui::TextEdit::singleline(&mut self.activation_path)
                            .hint_text("…\\steamapps\\common\\Game")
                            .margin(egui::Margin::symmetric(12, 10)),
                    );
                    if ui
                        .add_sized([120.0, 40.0], ghost_button("CHOOSE FOLDER"))
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        self.activation_path = folder.display().to_string();
                        self.ubisoft_activation_code.clear();
                    }
                });

                ui.add_space(14.0);
                ui.label(
                    RichText::new(
                        "Drydock adds the Ubisoft magicfiles, launches the game once, and reads the \
                         token_req.txt it generates. Close the game once the token request appears.",
                    )
                    .size(10.5)
                    .color(MUTED),
                );

                ui.add_space(12.0);
                let busy = self.ubisoft_prepare_receiver.is_some();
                let can_prepare = !self.activation_path.trim().is_empty() && !busy;
                if ui
                    .add_enabled(can_prepare, primary_button("PREPARE & LAUNCH GAME"))
                    .on_hover_text("Install magicfiles, launch the game, and capture its token request")
                    .clicked()
                {
                    self.start_ubisoft_prepare(app_id, PathBuf::from(self.activation_path.trim()));
                }
            } else if self.catalog.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the game catalog…").size(10.5).color(MUTED));
            }

            if !self.ubisoft_activation_code.is_empty() {
                ui.add_space(18.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(18, 21, 24))
                    .corner_radius(12)
                    .inner_margin(18)
                    .show(ui, |ui| {
                        section_label(ui, "ACTIVATION CODE");
                        ui.label(
                            RichText::new(&self.ubisoft_activation_code)
                                .size(22.0)
                                .strong()
                                .color(TEXT),
                        );
                        if ui
                            .add(ghost_button("COPY CODE"))
                            .on_hover_text("Copy the Ubisoft activation code to the clipboard")
                            .clicked()
                        {
                            ui.ctx().copy_text(self.ubisoft_activation_code.clone());
                            self.status = "Activation code copied".into();
                            self.status_error = false;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Send this code in your Ubisoft ticket. It expires after 30 minutes.",
                            )
                            .size(10.5)
                            .color(ACCENT),
                        );

                        ui.add_space(18.0);
                        section_label(ui, "RESPONSE CODE FROM THE BOT");
                        ui.add_space(7.0);
                        response_code_fields(ui, &mut self.response_code);
                        ui.add_space(12.0);
                        let response_is_complete = self.response_code.iter().all(|part| {
                            part.len() == 1 && part.chars().all(|value| value.is_ascii_alphanumeric())
                        });
                        if ui
                            .add_enabled(
                                response_is_complete && self.activation_verify_receiver.is_none(),
                                primary_button("ACTIVATE"),
                            )
                            .on_hover_text("Verify the response and install token.ini next to the game")
                            .clicked()
                            && let Some(app_id) = self.selected_app
                        {
                            self.verify_ubisoft_response(app_id);
                        }
                        if let Some(entitlement) = &self.verified_entitlement {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "TOKEN INSTALLED · {} bytes · valid until {}",
                                    entitlement.payload_bytes, entitlement.expires_utc
                                ))
                                .size(9.5)
                                .strong()
                                .color(ACCENT_SOFT),
                            );
                        }
                    });
            }
        });
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

