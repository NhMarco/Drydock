use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;

pub fn screenshot_gallery(ui: &mut egui::Ui, screenshots: &[String], index: usize) -> usize {
    let count = screenshots.len();
    let mut new_index = index.min(count - 1);
    let corner = egui::CornerRadius::same(16);

    // Main image spans the full column width at a native 16:9 frame, filled edge-to-edge (cover) so
    // no letterbox bars ever appear. Overlay controls sit on top of it.
    let full_width = ui.available_width();
    let image_h = full_width * 0.5625;
    let (image_rect, _) = ui.allocate_exact_size(Vec2::new(full_width, image_h), Sense::hover());
    paint_remote_image_cover(ui, image_rect, &screenshots[new_index], corner);

    if count > 1 {
        let radius = 20.0;
        let cy = image_rect.center().y;
        if overlay_arrow(
            ui,
            egui::pos2(image_rect.left() + radius + 16.0, cy),
            radius,
            false,
        ) {
            new_index = if new_index == 0 { count - 1 } else { new_index - 1 };
        }
        if overlay_arrow(
            ui,
            egui::pos2(image_rect.right() - radius - 16.0, cy),
            radius,
            true,
        ) {
            new_index = (new_index + 1) % count;
        }

        // A rounded "current / total" badge tucked into the image's bottom-right corner.
        let galley = ui.painter().layout_no_wrap(
            format!("{} / {count}", new_index + 1),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
        let badge_size = galley.size() + Vec2::new(20.0, 10.0);
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(
                image_rect.right() - 12.0 - badge_size.x,
                image_rect.bottom() - 12.0 - badge_size.y,
            ),
            badge_size,
        );
        ui.painter().rect_filled(
            badge_rect,
            egui::CornerRadius::same(255),
            Color32::from_black_alpha(170),
        );
        ui.painter()
            .galley(badge_rect.center() - galley.size() * 0.5, galley, Color32::WHITE);

        // Thumbnail filmstrip: scroll horizontally, click to jump, active one ringed in violet.
        ui.add_space(10.0);
        let thumb_h = 58.0;
        let thumb_w = thumb_h / 0.5625;
        let thumb_corner = egui::CornerRadius::same(8);
        egui::ScrollArea::horizontal()
            .id_salt("screenshot_strip")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (i, url) in screenshots.iter().enumerate() {
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(thumb_w, thumb_h), Sense::click());
                        paint_remote_image_cover(ui, rect, url, thumb_corner);
                        if i == new_index {
                            ui.painter().rect_stroke(
                                rect,
                                thumb_corner,
                                Stroke::new(2.0, ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        } else {
                            // Dim the ones you're not viewing; lift the dimming on hover.
                            let dim = if response.hovered() { 25 } else { 105 };
                            ui.painter()
                                .rect_filled(rect, thumb_corner, Color32::from_black_alpha(dim));
                            if response.hovered() {
                                ui.painter().rect_stroke(
                                    rect,
                                    thumb_corner,
                                    Stroke::new(1.5, lerp_color(BORDER, ACCENT, 0.6)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            new_index = i;
                        }
                    }
                });
            });
    }

    new_index
}

/// A small translucent circular navigation button overlaid on a screenshot edge. Returns whether
/// it was clicked.
pub fn overlay_arrow(ui: &mut egui::Ui, center: egui::Pos2, radius: f32, forward: bool) -> bool {
    let rect = egui::Rect::from_center_size(center, Vec2::splat(radius * 2.0));
    let response = ui.interact(rect, ui.id().with(("ss_arrow", forward)), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    ui.painter().circle_filled(
        center,
        radius,
        Color32::from_black_alpha((150.0 + 80.0 * hover) as u8),
    );
    ui.painter().circle_stroke(
        center,
        radius,
        Stroke::new(1.0, lerp_color(Color32::from_white_alpha(45), ACCENT, hover)),
    );
    let dx = if forward { 3.5 } else { -3.5 };
    ui.painter().add(egui::Shape::line(
        vec![
            egui::pos2(center.x - dx, center.y - 7.0),
            egui::pos2(center.x + dx, center.y),
            egui::pos2(center.x - dx, center.y + 7.0),
        ],
        Stroke::new(2.2, Color32::WHITE),
    ));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// Visual weight of a [`PillButton`].

pub fn detail_field(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).size(8.5).strong().color(ACCENT));
    ui.add(egui::Label::new(RichText::new(value).size(11.0).color(TEXT)).wrap());
    ui.add_space(10.0);
}

/// Renders the whole details body inside a fixed-width column. Returns whether the Steam
/// action was clicked and the (possibly advanced) screenshot index. Kept free of `self` so
/// it can run inside the centring layout closures without borrow conflicts.
/// Native-depot-download button state for the details sidebar.

#[allow(clippy::too_many_arguments)]
pub fn details_body(
    ui: &mut egui::Ui,
    details: &SteamStoreDetails,
    state: DetailsState,
    width: f32,
    shot_index: usize,
    activation_required: bool,
    panels: DetailsPanels,
    depot: DepotButtons,
) -> (DetailsAction, usize) {
    let gap = 20.0;
    let mut action = DetailsAction::None;
    let mut new_shot = shot_index;

    // The game title heads the page, Steam-store style.
    ui.add(egui::Label::new(RichText::new(&details.name).size(30.0).strong().color(TEXT)).wrap());
    ui.label(
        RichText::new(format!("APP {}", details.app_id))
            .size(10.0)
            .color(ACCENT),
    );
    ui.add_space(16.0);

    // Two flowing columns so neither side leaves a big empty gap: the left stacks the media carousel
    // over "About this game"; the right stacks the store sidebar over Features & DRM and the system
    // requirements. On narrow windows everything collapses into one column.
    if width >= 900.0 {
        let right_w = (width * 0.35).clamp(320.0, 440.0);
        let left_w = width - right_w - gap;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(left_w, 10.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left_w);
                new_shot = details_media(ui, details, shot_index);
                ui.add_space(24.0);
                details_about(ui, details);
            });
            ui.add_space(gap);
            ui.allocate_ui_with_layout(Vec2::new(right_w, 10.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(right_w);
                action = details_store_sidebar(ui, details, state, activation_required, panels, depot);
                ui.add_space(24.0);
                details_features(ui, details, activation_required);
                ui.add_space(24.0);
                details_requirements(ui, details);
            });
        });
    } else {
        new_shot = details_media(ui, details, shot_index);
        ui.add_space(16.0);
        action = details_store_sidebar(ui, details, state, activation_required, panels, depot);
        ui.add_space(24.0);
        details_about(ui, details);
        ui.add_space(24.0);
        details_features(ui, details, activation_required);
        ui.add_space(24.0);
        details_requirements(ui, details);
    }
    (action, new_shot)
}

/// The details media column: the screenshot carousel, or the header art when a game has no shots.
pub fn details_media(ui: &mut egui::Ui, details: &SteamStoreDetails, shot_index: usize) -> usize {
    if details.screenshots.is_empty() {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width * STEAM_HEADER_ASPECT), Sense::hover());
        paint_remote_image(
            ui,
            rect,
            &details.header_image_url,
            egui::CornerRadius::same(12),
            "ARTWORK UNAVAILABLE",
        );
        return shot_index;
    }
    screenshot_gallery(ui, &details.screenshots, shot_index)
}

/// The Steam-style store sidebar: the header capsule, the short description, the facts block
/// (reviews / release / developer / publisher), the tag chips, and the Drydock action buttons.
pub fn details_store_sidebar(
    ui: &mut egui::Ui,
    details: &SteamStoreDetails,
    state: DetailsState,
    activation_required: bool,
    panels: DetailsPanels,
    depot: DepotButtons,
) -> DetailsAction {
    let mut action = DetailsAction::None;
    panel(ui, |ui| {
        let inner = ui.available_width();
        if !details.header_image_url.is_empty() {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(inner, inner * STEAM_HEADER_ASPECT), Sense::hover());
            paint_remote_image(
                ui,
                rect,
                &details.header_image_url,
                egui::CornerRadius::same(8),
                "",
            );
            ui.add_space(12.0);
        }
        if !details.short_description.is_empty() {
            ui.add(egui::Label::new(RichText::new(&details.short_description).size(12.0).color(TEXT)).wrap());
            ui.add_space(14.0);
        }

        sidebar_fact(ui, "ALL REVIEWS", &details.reviews.display_text(), ACCENT_SOFT);
        sidebar_fact(ui, "RELEASED", &value_or_unknown(&details.release_date), TEXT);
        sidebar_fact(ui, "DEVELOPER", &joined_or_unknown(&details.developers), ACCENT);
        sidebar_fact(ui, "PUBLISHER", &joined_or_unknown(&details.publishers), ACCENT);
        if activation_required {
            let notice = details.drm_notice.trim();
            let text = if notice.is_empty() {
                "Denuvo — activation required".to_owned()
            } else {
                format!("{notice} — activation required")
            };
            sidebar_fact(ui, "DRM", &text, AMBER);
        }

        if !details.genres.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("TAGS").size(8.5).strong().color(MUTED));
            ui.add_space(6.0);
            tag_chip_flow(ui, &details.genres);
        }

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(12.0);
        // The Drydock action hub (Add to Steam / Activate / Apply Fix / Download).
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            action = steam_button_row(ui, state, panels, activation_required);
        });

        // Native depot download: real game files via manifest + key, shown only when the proxy has
        // download data for this app.
        if depot.downloadable {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                // Enabled even while another download runs — it just goes into the queue and starts
                // when the current one finishes (enqueue_download won't add the same game twice).
                let download_label = if depot.busy {
                    "ADD TO QUEUE"
                } else if depot.installed {
                    "DOWNLOAD / REPAIR"
                } else {
                    "DOWNLOAD IN DRYDOCK"
                };
                let download_hint = if depot.busy {
                    "Queue this game — it starts once the current download finishes"
                } else {
                    "Download the real game files into your Steam library (manifest + depot key → Steam CDN)"
                };
                if ui
                    .add(success_button(download_label))
                    .on_hover_text(download_hint)
                    .clicked()
                {
                    action = DetailsAction::DepotDownload;
                }
                if depot.installed
                    && ui
                        .add_enabled(!depot.busy, ghost_button("VERIFY FILES"))
                        .on_hover_text("Check the installed files against the depot manifest")
                        .clicked()
                {
                    action = DetailsAction::DepotVerify;
                }
            });
        }
    });
    action
}

/// One label + value line in the store sidebar, Steam-store style: a fixed-width right-aligned grey
/// label column and a value column that starts at the same x on every row and wraps if needed.
pub fn sidebar_fact(ui: &mut egui::Ui, label: &str, value: &str, value_color: Color32) {
    pub const LABEL_W: f32 = 96.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        // Left-aligned label column of a fixed width, so labels sit flush left (aligned with TAGS)
        // and all values still line up in one column.
        ui.allocate_ui_with_layout(Vec2::new(LABEL_W, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(LABEL_W);
            ui.add_space(1.0);
            ui.add(egui::Label::new(RichText::new(label).size(9.5).color(MUTED)));
        });
        ui.add(egui::Label::new(RichText::new(value).size(11.5).color(value_color)).wrap());
    });
    ui.add_space(8.0);
}

/// Lays out tag chips across as many rows as needed, breaking a row before a chip that would
/// overflow the available width. egui's `horizontal_wrapped` squeezes an over-wide Frame chip into
/// the leftover space (which then clips or char-wraps) instead of moving it to the next row, so the
/// row breaks are computed here from each chip's measured width.
pub fn tag_chip_flow(ui: &mut egui::Ui, tags: &[String]) {
    pub const GAP: f32 = 6.0;
    pub const CHIP_PAD: f32 = 9.0 * 2.0 + 2.0; // symmetric inner margin + stroke
    let avail = ui.available_width();

    let mut rows: Vec<Vec<&str>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for tag in tags.iter().take(14) {
        let galley = ui
            .painter()
            .layout_no_wrap(tag.clone(), FontId::proportional(10.5), TEXT);
        let w = galley.size().x + CHIP_PAD;
        let row = rows.last_mut().expect("one row always present");
        if !row.is_empty() && used + GAP + w > avail {
            rows.push(vec![tag.as_str()]);
            used = w;
        } else {
            if !row.is_empty() {
                used += GAP;
            }
            used += w;
            row.push(tag.as_str());
        }
    }

    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            ui.add_space(GAP);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for tag in row {
                tag_chip(ui, tag);
            }
        });
    }
}

/// A rounded genre/tag pill for the store sidebar.
pub fn tag_chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(lerp_color(SURFACE_RAISED, ACCENT_SOFT, 0.10))
        .stroke(Stroke::new(1.0, lerp_color(BORDER, ACCENT_SOFT, 0.30)))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            // Keep the chip on one line: never let the label wrap character-by-character. If it
            // doesn't fit the row, the flow layout moves the whole chip to the next line.
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .size(10.5)
                        .color(lerp_color(TEXT, ACCENT_SOFT, 0.4)),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

/// The features / DRM rail beside "About this game": platforms, Metacritic and the DRM notice.
pub fn details_features(ui: &mut egui::Ui, details: &SteamStoreDetails, activation_required: bool) {
    section_label(ui, "FEATURES & DRM");
    ui.add_space(8.0);
    panel(ui, |ui| {
        detail_field(ui, "PLATFORMS", joined_or_unknown(&details.platforms));
        detail_field(
            ui,
            "METACRITIC",
            details
                .metacritic_score
                .map_or_else(|| "Not rated".to_owned(), |score| format!("{score} / 100")),
        );
        let drm = if activation_required {
            let notice = details.drm_notice.trim();
            if notice.is_empty() {
                "Denuvo Anti-Tamper — Drydock activation required".to_owned()
            } else {
                format!("{notice} — Drydock activation required")
            }
        } else {
            "None — no activation needed".to_owned()
        };
        ui.label(RichText::new("THIRD-PARTY DRM").size(8.5).strong().color(ACCENT));
        ui.add(
            egui::Label::new(RichText::new(drm).size(11.0).color(if activation_required {
                AMBER
            } else {
                TEXT
            }))
            .wrap(),
        );
    });
}

/// A button the user pressed on an app's details page.


/// Renders the details-page action buttons (the hub for a game) and returns the one that was
/// clicked: add the latest or cracked unlock, apply the Denuvo fix, download a repack, or jump
/// to Activation.
pub fn steam_button_row(
    ui: &mut egui::Ui,
    state: DetailsState,
    panels: DetailsPanels,
    activation_required: bool,
) -> DetailsAction {
    let mut action = DetailsAction::None;
    let installed = panels.fix.is_some_and(|fix| fix.installed);
    let busy_fix = panels.fix.is_some_and(|fix| fix.busy);
    let has_denuvo = panels.fix.is_some_and(|fix| fix.denuvo.is_some());

    // Add-to-Steam workflow: latest and (when a Denuvo fix exists) cracked, or Update/Remove once added.
    if state.is_added {
        if ui
            .add_enabled(!state.busy, primary_button("UPDATE"))
            .on_hover_text("Re-fetch and re-install the latest unlock Lua")
            .clicked()
        {
            action = DetailsAction::AddToSteam;
        }
        if ui
            .add_enabled(!state.busy, ghost_button("REMOVE FROM STEAM"))
            .clicked()
        {
            action = DetailsAction::RemoveFromSteam;
        }
    } else if state.service_current {
        if ui
            .add_enabled(!state.busy, primary_button("ADD LATEST VERSION TO STEAM"))
            .on_hover_text("Add the normal unlock so Steam installs the latest build")
            .clicked()
        {
            action = DetailsAction::AddToSteam;
        }
        if has_denuvo
            && ui
                .add_enabled(!state.busy, primary_button("ADD CRACKED VERSION TO STEAM"))
                .on_hover_text("Add the build-locked unlock that pins the game to the cracked build")
                .clicked()
        {
            action = DetailsAction::AddCracked;
        }
    } else {
        // The Steam Service must be installed before any app can be added, so offer to install it
        // right here instead of just disabling the button.
        let response = ui
            .add_enabled(!state.busy, primary_button("INSTALL STEAM SERVICE"))
            .on_hover_text(
                "The Steam Service must be installed before adding games. Click to install it now.",
            );
        if response.clicked() {
            action = DetailsAction::InstallService;
        }
    }

    if let Some(fix) = panels.fix {
        // Denuvo fix (GitHub build-locked Lua + zip), labelled with its installed status.
        if let Some(status) = fix.denuvo {
            let text = match status {
                FixStatus::Applied => "RE-APPLY DENUVO FIX",
                FixStatus::IncompatibleLua | FixStatus::NotApplied => "APPLY DENUVO FIX",
            };
            let response = ui.add_enabled(installed && !busy_fix, ghost_button(text));
            let response = if !installed {
                response.on_hover_text("Install the game through Steam first.")
            } else if busy_fix {
                response.on_hover_text("A background action is already running.")
            } else {
                match status {
                    FixStatus::Applied => response.on_hover_text(
                        "The verified fix Lua is installed. Re-apply to refresh the game files.",
                    ),
                    FixStatus::IncompatibleLua => response.on_hover_text(
                        "A different Lua is installed for this app — applying replaces it with the verified fix Lua.",
                    ),
                    FixStatus::NotApplied => response,
                }
            };
            if response.clicked() {
                action = DetailsAction::ApplyDenuvoFix;
            }
        }
    }

    // Download the game as an external repack — one button per repacker source.
    if let Some(repack) = panels.repack {
        let single = repack.repackers.len() == 1;
        for (index, repacker) in repack.repackers.iter().enumerate() {
            let label = if single {
                "DOWNLOAD REPACK".to_owned()
            } else {
                format!("DOWNLOAD REPACK ({repacker})")
            };
            if ui
                .add(ghost_button(&label))
                .on_hover_text(format!("Open {repacker} in your browser"))
                .clicked()
            {
                action = DetailsAction::Download(index);
            }
        }
    }

    // Activate — only for Denuvo games; opens the Activation tab with this game preselected.
    if activation_required
        && ui
            .add(ghost_button("ACTIVATE"))
            .on_hover_text("Open Activation with this game preselected")
            .clicked()
    {
        action = DetailsAction::Activate;
    }
    action
}

pub fn details_about(ui: &mut egui::Ui, details: &SteamStoreDetails) {
    section_label(ui, "ABOUT THIS GAME");
    ui.add_space(8.0);
    panel(ui, |ui| {
        let empty = details.about_the_game.is_empty();
        // Show the full description at its natural height (no inner scrollbar); the page scrolls.
        ui.add(
            egui::Label::new(
                RichText::new(if empty {
                    "No description is available."
                } else {
                    &details.about_the_game
                })
                .size(12.0)
                .color(if empty { MUTED } else { TEXT }),
            )
            .wrap(),
        );
    });
}

pub fn details_requirements(ui: &mut egui::Ui, details: &SteamStoreDetails) {
    section_label(ui, "SYSTEM REQUIREMENTS");
    ui.add_space(8.0);
    // In the narrow right column the two side-by-side columns get cramped, so stack them there.
    let stacked = ui.available_width() < 520.0;
    panel(ui, |ui| {
        if stacked {
            requirement_column(ui, "MINIMUM", &details.requirements.minimum);
            ui.add_space(12.0);
            requirement_column(ui, "RECOMMENDED", &details.requirements.recommended);
        } else {
            ui.columns(2, |columns| {
                requirement_column(&mut columns[0], "MINIMUM", &details.requirements.minimum);
                requirement_column(&mut columns[1], "RECOMMENDED", &details.requirements.recommended);
            });
        }
    });
}

pub fn requirement_column(ui: &mut egui::Ui, heading: &str, value: &str) {
    ui.label(RichText::new(heading).size(10.0).strong().color(ACCENT));
    ui.add_space(6.0);
    ui.label(
        RichText::new(if value.is_empty() { "Not specified" } else { value })
            .size(10.5)
            .color(if value.is_empty() { MUTED } else { TEXT }),
    );
}


impl DrydockApp {
    pub fn open_details(&mut self, app_id: u32) {
        self.page = Page::Details;
        self.details_app_id = Some(app_id);
        self.store_details = None;
        self.store_loading = true;
        self.screenshot_index = 0;

        let cache_directory = self.paths.cache_dir().join("store-details");
        let (sender, receiver) = mpsc::channel();
        self.store_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = SteamStoreClient::new(cache_directory)
                .and_then(|client| client.details(app_id))
                .map_err(|error| error.to_string());
            let _ = sender.send((app_id, result));
        });
    }

    /// Starts a background depot download (or verify) into the Steam library. Only one job runs at a
    /// time. The install root is the installed game's folder when present, otherwise
    /// `steamapps/common/<installdir>` under the main Steam library (installdir resolved on the thread).
    /// Whether a background download/verify thread is currently running.
    pub fn poll_store_details(&mut self) {
        let Some(receiver) = self.store_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok((app_id, result)) => {
                self.store_receiver = None;
                if self.details_app_id != Some(app_id) {
                    return;
                }
                self.store_loading = false;
                match result {
                    Ok(details) => {
                        self.status = if details.stale_cache {
                            format!(
                                "Loaded cached {} details; Steam Store is unavailable",
                                details.name
                            )
                        } else {
                            format!("Loaded {}", details.name)
                        };
                        self.status_error = details.stale_cache;
                        self.store_details = Some(details);
                    }
                    Err(error) => {
                        self.status = format!("Store details could not be loaded: {error}");
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.store_receiver = None;
                self.store_loading = false;
                self.status = "The store request ended unexpectedly".into();
                self.status_error = true;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Kicks off the live storefront fetch (Steam's `featuredcategories`) unless one is already in
    /// flight or a fresh result is in hand. Called when the Store page first needs it.
    /// Builds the Apply-Fix panel for the details page when the app has a build-locked Denuvo fix
    /// (from the GitHub MFB repo). The DepotBox "online fix" API path has been removed.
    pub fn details_fix_panel(&self, app_id: u32) -> Option<FixPanelState> {
        let fix = self.fix_for(app_id)?;
        let denuvo = fix.denuvo.as_ref()?;
        // The Denuvo fix's applied status needs the Steam folder to inspect the plug-in Lua.
        let status = self
            .steam
            .root
            .as_deref()
            .map_or(FixStatus::NotApplied, |root| fix_status(root, denuvo));
        Some(FixPanelState {
            busy: self.background_action.is_some(),
            installed: self.manifests.iter().any(|manifest| manifest.app_id == app_id),
            denuvo: Some(status),
        })
    }

    /// Builds the repack Download-button panel for the details page when the app has repack sources.
    pub fn details_repack_panel(&self, app_id: Option<u32>) -> Option<RepackPanelState> {
        let repack = self.repack_for(app_id?)?;
        Some(RepackPanelState {
            repackers: repack
                .sources
                .iter()
                .map(|source| source.repacker.clone())
                .collect(),
        })
    }

    pub fn details_state(&self, _manifest: &Option<SteamManifest>) -> DetailsState {
        DetailsState {
            is_added: self
                .details_app_id
                .is_some_and(|app_id| self.is_app_added(app_id)),
            service_current: matches!(
                self.service_status.as_ref().map(|status| status.state),
                Some(SteamServiceState::Current)
            ),
            busy: self.service_receiver.is_some(),
        }
    }

    /// Dispatches a details-page button click to the matching Steam or unlock operation.
    pub fn perform_details_action(&mut self, app_id: u32, name: &str, action: DetailsAction) {
        match action {
            DetailsAction::None => {}
            DetailsAction::AddToSteam => self.add_app_to_steam(app_id),
            DetailsAction::AddCracked => self.add_cracked_to_steam(app_id),
            DetailsAction::RemoveFromSteam => self.remove_app_from_steam(app_id),
            DetailsAction::InstallService => self.install_steam_service(),
            DetailsAction::ApplyDenuvoFix => self.apply_denuvo_fix_for(app_id),
            DetailsAction::Download(index) => self.open_repack_source(app_id, index),
            DetailsAction::Activate => self.go_to_activation(app_id),
            DetailsAction::DepotDownload => self.enqueue_download(app_id, name.to_owned()),
            DetailsAction::DepotVerify => self.start_verify(app_id, name.to_owned()),
        }
    }

    /// Opens the chosen repack source's link in the user's browser (http(s) only).
    pub fn open_repack_source(&mut self, app_id: u32, index: usize) {
        let Some(source) = self
            .repack_for(app_id)
            .and_then(|repack| repack.sources.get(index))
        else {
            return;
        };
        let repacker = source.repacker.clone();
        let link = source.link.clone();
        match open_link(&link) {
            Ok(()) => {
                self.status = format!("Opening {repacker} in your browser");
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    pub fn details_page(&mut self, ui: &mut egui::Ui) {
        if back_button(ui, "Return to search").clicked() {
            self.page = Page::Home;
            return;
        }
        ui.add_space(18.0);

        let manifest = self
            .details_app_id
            .and_then(|app_id| self.manifests.iter().find(|app| app.app_id == app_id))
            .cloned();
        let fallback_name = self
            .details_app_id
            .and_then(|app_id| self.catalog.iter().find(|app| app.app_id == app_id))
            .map(|app| app.name.clone())
            .or_else(|| manifest.as_ref().map(|app| app.name.clone()))
            .unwrap_or_else(|| "GAME DETAILS".to_owned());

        if self.store_loading {
            page_heading(ui, &fallback_name);
            ui.add_space(36.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading details and artwork").color(MUTED));
            });
            return;
        }

        let state = self.details_state(&manifest);
        let app_id = self.details_app_id;

        let Some(details) = self.store_details.as_ref() else {
            page_heading(ui, &fallback_name);
            ui.add_space(20.0);
            let fix = app_id.and_then(|id| self.details_fix_panel(id));
            let repack = self.details_repack_panel(app_id);
            let mut action = DetailsAction::None;
            panel(ui, |ui| {
                ui.label(RichText::new("Store information is currently unavailable.").color(MUTED));
                if let Some(app) = &manifest {
                    ui.label(RichText::new(format!("APP {}", app.app_id)).color(ACCENT));
                    ui.label(RichText::new(app.install_dir().display().to_string()).color(MUTED));
                } else if let Some(id) = app_id {
                    ui.label(RichText::new(format!("APP {id}")).color(ACCENT));
                    ui.label(RichText::new("Not installed through Steam").color(MUTED));
                }
                if let Some(id) = app_id {
                    let activation_required = self.needs_activation(id) == Some(true);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        action = steam_button_row(
                            ui,
                            state,
                            DetailsPanels {
                                fix: fix.as_ref(),
                                repack: repack.as_ref(),
                            },
                            activation_required,
                        );
                    });
                }
            });
            if let Some(id) = app_id {
                self.perform_details_action(id, &fallback_name, action);
            }
            return;
        };

        // Centre the details in a comfortable max-width column: symmetric margins on wide
        // screens read as intentional, and text lines stay readable instead of stretching
        // edge to edge.
        // The page is already centred and width-capped by the shell, so render at full column width.
        let content_width = ui.available_width();
        let shot = self.screenshot_index;
        // Activation is offered only for real Denuvo games: the Steam "Denuvo Watch" curator, or
        // Steam's own store DRM notice for this app (`uses_denuvo`). The MFB fix list never factors
        // in here — it drives only the separate "Add cracked version" button.
        let activation_required = self.needs_activation(details.app_id) == Some(true) || details.uses_denuvo;
        // The Apply Fix buttons only appear for details opened from the Fixes tab, and only when
        // a fix exists for this app (one button per variant).
        let fix = self.details_fix_panel(details.app_id);
        let repack = self.details_repack_panel(Some(details.app_id));
        let depot = DepotButtons {
            // Depot availability isn't cheaply probeable, so offer Download for any real game and
            // report "no depot data" at download time if the package turns out to be missing.
            downloadable: is_real_game(details.app_id, &details.name),
            installed: self
                .manifests
                .iter()
                .any(|manifest| manifest.app_id == details.app_id),
            busy: self
                .download_job
                .as_ref()
                .is_some_and(|job| job.finished.is_none()),
        };
        let (action, new_shot) = details_body(
            ui,
            details,
            state,
            content_width,
            shot,
            activation_required,
            DetailsPanels {
                fix: fix.as_ref(),
                repack: repack.as_ref(),
            },
            depot,
        );
        self.screenshot_index = new_shot;
        let name = details.name.clone();
        self.perform_details_action(details.app_id, &name, action);
    }

}

