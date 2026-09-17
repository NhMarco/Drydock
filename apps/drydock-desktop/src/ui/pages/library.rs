use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

/// Dimensions for portrait library poster cards (matching Steam's 600x900 aspect ratio).
pub const POSTER_CARD_W: f32 = 176.0;
pub const POSTER_CARD_H: f32 = 264.0;

/// One portrait poster card in the Library grid view.
pub fn library_poster_card(ui: &mut egui::Ui, entry: &LibraryEntry) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(POSTER_CARD_W, POSTER_CARD_H), Sense::click());
    let hover = ui.ctx().animate_bool(resp.id, resp.hovered());

    let stroke = Stroke::new(1.5, lerp_color(BORDER, ACCENT, hover));
    let fill = lerp_color(SURFACE, SURFACE_RAISED, hover);

    // Card background with rounded corners
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(12),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );

    // Artwork area (top part of the card)
    let art_h = POSTER_CARD_H - 54.0;
    let art_rect = egui::Rect::from_min_size(rect.min, Vec2::new(POSTER_CARD_W, art_h));

    let urls = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_600x900.jpg",
            entry.app_id
        ),
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
            entry.app_id
        ),
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            entry.app_id
        ),
    ];
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    paint_remote_image_cover_multi(
        ui,
        art_rect,
        &refs,
        egui::CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        },
    );

    // Subtle dark gradient fade at the bottom of the artwork
    let fade_h = 40.0;
    let fade_rect = egui::Rect::from_min_max(
        egui::pos2(art_rect.left(), art_rect.bottom() - fade_h),
        art_rect.max,
    );
    ui.painter().with_clip_rect(art_rect).rect_filled(
        fade_rect,
        0,
        Color32::from_rgba_unmultiplied(13, 27, 40, 210),
    );

    // Top-right status pill/dot
    let (status_color, status_text) = match entry.source {
        LibrarySource::SteamInstalled => (VERDIGRIS, "INSTALLED"),
        LibrarySource::DrydockInstalled => (ACCENT, "STANDALONE"),
        LibrarySource::Available => (AMBER, "AVAILABLE"),
    };

    // Sleek dot indicator with dark backing circle
    let dot_center = egui::pos2(rect.right() - 14.0, rect.top() + 14.0);
    ui.painter()
        .circle_filled(dot_center, 6.0, Color32::from_rgba_unmultiplied(6, 12, 20, 210));
    ui.painter().circle_filled(dot_center, 4.0, status_color);

    // Title area at bottom of card
    let title_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 10.0, art_rect.bottom() + 6.0),
        egui::pos2(rect.right() - 10.0, rect.bottom() - 6.0),
    );
    let title_color = if !entry.installed {
        lerp_color(MUTED, TEXT, hover)
    } else {
        lerp_color(TEXT, ACCENT_SOFT, hover)
    };

    ui.painter().with_clip_rect(title_rect).text(
        egui::pos2(title_rect.left(), title_rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        &entry.name,
        FontId::proportional(14.0),
        title_color,
    );

    let clicked = resp.clicked();
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        resp.on_hover_ui(|ui| {
            ui.label(RichText::new(&entry.name).size(14.0).strong().color(TEXT));
            ui.label(
                RichText::new(format!("● {status_text}"))
                    .size(14.0)
                    .color(status_color),
            );
            if let Some(size) = entry.size_on_disk {
                ui.label(
                    RichText::new(format!("Size: {}", human_bytes(size)))
                        .size(14.0)
                        .color(MUTED),
                );
            }
        });
    }

    clicked
}

/// The right-hand Library overview of the highlighted game: a full-width header banner with
/// gradient feathering, large title, status badges, elevated action bar, and bento metadata cards.
pub fn library_overview(
    ui: &mut egui::Ui,
    entry: &LibraryEntry,
    catalog: &[CatalogApp],
    modal_w: f32,
    _modal_h: f32,
    close_requested: &mut bool,
) -> Option<LibraryAction> {
    let mut action = None;
    let hero_h = 260.0;

    // ── Hero banner ──────────────────────────────────────────────────────────
    // Build rect from the window's actual inner content origin.
    let cursor_origin = ui.cursor().min;
    let inner_left = ui.max_rect().left();
    let inner_top = cursor_origin.y;
    let hero = egui::Rect::from_min_size(egui::pos2(inner_left, inner_top), Vec2::new(modal_w, hero_h));

    // Claim layout space so subsequent widgets render below.
    ui.allocate_rect(
        egui::Rect::from_min_size(cursor_origin, Vec2::new(ui.available_width(), hero_h)),
        egui::Sense::hover(),
    );

    // Background image
    let urls = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
            entry.app_id
        ),
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            entry.app_id
        ),
    ];
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let corner = egui::CornerRadius {
        nw: 16,
        ne: 16,
        sw: 0,
        se: 0,
    };
    let hero_clip = ui.max_rect().union(hero);
    let layer_painter = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(hero_clip);
    layer_painter.rect_filled(hero, corner, SURFACE);
    let old_clip = ui.clip_rect();
    ui.set_clip_rect(hero_clip);
    paint_remote_image_cover_multi(ui, hero, &refs, corner);
    ui.set_clip_rect(old_clip);

    // Deep gradient: covers bottom 60% of banner → fades to SURFACE
    let painter = ui.painter().with_clip_rect(hero_clip);
    let steps = 18;
    let fade_h = hero_h * 0.65;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let y0 = hero.bottom() - fade_h * (1.0 - t0);
        let y1 = hero.bottom() - fade_h * (1.0 - t1);
        let a = (t1.powf(1.6) * 248.0) as u8;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(hero.left(), y0), egui::pos2(hero.right(), y1)),
            0,
            Color32::from_rgba_unmultiplied(13, 27, 40, a),
        );
    }

    // ── Title + status overlaid at bottom of hero ────────────────────────────
    let text_x = hero.left() + 20.0;
    let text_bottom = hero.bottom() - 16.0;

    // Status dot + text
    let (status_label, status_color) = match entry.source {
        LibrarySource::SteamInstalled => ("Installed via Steam", VERDIGRIS),
        LibrarySource::DrydockInstalled if entry.launch_path.is_some() => {
            ("Installed via Drydock · Ready", ACCENT_SOFT)
        }
        LibrarySource::DrydockInstalled => ("Installed · No launcher linked", AMBER),
        LibrarySource::Available => ("Unlock Active · Ready to Install", AMBER),
    };

    // Status line: dot + text share the same vertical center
    let status_cy = text_bottom - 10.0; // center Y of the status line
    painter.circle_filled(egui::pos2(text_x + 4.0, status_cy), 3.5, status_color);
    painter.text(
        egui::pos2(text_x + 12.0, status_cy),
        egui::Align2::LEFT_CENTER,
        status_label,
        egui::FontId::proportional(12.5),
        status_color,
    );

    // Title — bottom edge sits 8px above the status center
    painter.text(
        egui::pos2(text_x, status_cy - 8.0),
        egui::Align2::LEFT_BOTTOM,
        &entry.name,
        egui::FontId::proportional(22.0),
        TEXT,
    );

    // APP ID badge (top-left)
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(hero.left() + 14.0, hero.top() + 14.0),
        Vec2::new(78.0, 22.0),
    );
    painter.rect_filled(
        badge_rect,
        egui::CornerRadius::same(5),
        Color32::from_black_alpha(160),
    );
    painter.rect_stroke(
        badge_rect,
        egui::CornerRadius::same(5),
        Stroke::new(1.0, Color32::from_white_alpha(30)),
        egui::StrokeKind::Middle,
    );
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("APP {}", entry.app_id),
        egui::FontId::proportional(11.0),
        Color32::from_white_alpha(140),
    );

    // Close button (top-right)
    let cb_r = 14.0;
    let cb_c = egui::pos2(hero.right() - cb_r - 14.0, hero.top() + cb_r + 12.0);
    let cb_id = ui.id().with("hero_close");
    let cb_rect = egui::Rect::from_center_size(cb_c, Vec2::splat(cb_r * 2.0));
    let cb_resp = ui.interact(cb_rect, cb_id, Sense::click());
    let (cb_fill, cb_stroke_col) = if cb_resp.hovered() {
        (
            Color32::from_rgba_unmultiplied(210, 40, 40, 230),
            Color32::from_white_alpha(230),
        )
    } else {
        (Color32::from_black_alpha(170), Color32::from_white_alpha(60))
    };
    painter.circle_filled(cb_c, cb_r, cb_fill);
    painter.circle_stroke(cb_c, cb_r, Stroke::new(1.0, cb_stroke_col));
    painter.text(
        cb_c,
        egui::Align2::CENTER_CENTER,
        icons::CLOSE,
        egui::FontId::proportional(12.0),
        Color32::WHITE,
    );
    if cb_resp.clicked() {
        *close_requested = true;
    }

    // ── Body (scrollable) ────────────────────────────────────────────────────
    let body_w = modal_w - 32.0;

    egui::ScrollArea::vertical()
        .id_salt("modal_body_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 16,
                    right: 16,
                    top: 16,
                    bottom: 14,
                })
                .show(ui, |ui| {
                    ui.set_width(body_w);

                    // ── Genre tags ───────────────────────────────────────────
                    if let Some(cat) = catalog
                        .iter()
                        .find(|c| c.app_id == entry.app_id && !c.tags.is_empty())
                    {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                            for tag in cat.tags.iter().take(5) {
                                status_pill(ui, tag, MUTED);
                            }
                        });
                        ui.add_space(14.0);
                    }

                    // ── Action buttons ───────────────────────────────────────
                    egui::Frame::new()
                        .fill(SURFACE_RAISED)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                                let mut click = |a: LibraryAction| action = Some(a);

                                match entry.source {
                                    LibrarySource::Available => {
                                        if ui
                                            .add(
                                                primary_button(&format!("{}  INSTALL", icons::DOWNLOAD))
                                                    .compact(),
                                            )
                                            .clicked()
                                        {
                                            click(LibraryAction::InstallSteam(entry.app_id));
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  UPDATE LUA", icons::UPDATES))
                                                    .compact(),
                                            )
                                            .on_hover_text("Re-fetch and re-install the unlock Lua")
                                            .clicked()
                                        {
                                            click(LibraryAction::UpdateLua(entry.app_id));
                                        }
                                        if ui
                                            .add(ghost_button(&format!("{}  STORE", icons::STORE)).compact())
                                            .clicked()
                                        {
                                            click(LibraryAction::Details(entry.app_id));
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  REMOVE LUA", icons::CLOSE))
                                                    .compact(),
                                            )
                                            .on_hover_text("Delete the unlock Lua from Steam")
                                            .clicked()
                                        {
                                            click(LibraryAction::RemoveLua(entry.app_id));
                                        }
                                    }
                                    LibrarySource::SteamInstalled => {
                                        if ui
                                            .add(success_button(&format!("{}  PLAY", icons::PLAY)).compact())
                                            .clicked()
                                        {
                                            click(LibraryAction::Launch(entry.app_id));
                                        }
                                        if let Some(dir) = &entry.install_dir {
                                            let clicked = ui
                                                .add(
                                                    ghost_button(&format!("{}  BROWSE", icons::FOLDER))
                                                        .compact(),
                                                )
                                                .on_hover_text("Open installation folder")
                                                .clicked();
                                            if clicked {
                                                click(LibraryAction::OpenFolder(dir.clone()));
                                            }
                                        }
                                        if ui
                                            .add(ghost_button(&format!("{}  STORE", icons::STORE)).compact())
                                            .on_hover_text("Open Steam Store page")
                                            .clicked()
                                        {
                                            click(LibraryAction::Details(entry.app_id));
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  UPDATE LUA", icons::UPDATES))
                                                    .compact(),
                                            )
                                            .on_hover_text("Re-fetch and re-install the unlock Lua")
                                            .clicked()
                                        {
                                            click(LibraryAction::UpdateLua(entry.app_id));
                                        }
                                        if ui
                                            .add(ghost_button("REMOVE LUA").compact())
                                            .on_hover_text("Delete the unlock Lua from Steam")
                                            .clicked()
                                        {
                                            click(LibraryAction::RemoveLua(entry.app_id));
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  UNINSTALL", icons::CLOSE))
                                                    .compact(),
                                            )
                                            .on_hover_text("Ask Steam to uninstall the game")
                                            .clicked()
                                        {
                                            click(LibraryAction::UninstallSteam(entry.app_id));
                                        }
                                    }
                                    LibrarySource::DrydockInstalled => {
                                        if entry.launch_path.is_some() {
                                            let clicked = ui
                                                .add(
                                                    success_button(&format!("{}  PLAY", icons::PLAY))
                                                        .compact(),
                                                )
                                                .clicked();
                                            if clicked {
                                                click(LibraryAction::Launch(entry.app_id));
                                            }
                                        } else if ui
                                            .add(
                                                primary_button(&format!("{}  SET .EXE", icons::SETTINGS))
                                                    .compact(),
                                            )
                                            .on_hover_text("Link the game's .exe so PLAY can launch it")
                                            .clicked()
                                        {
                                            click(LibraryAction::SetExe(entry.app_id));
                                        }
                                        if let Some(dir) = &entry.install_dir {
                                            let clicked = ui
                                                .add(
                                                    ghost_button(&format!("{}  BROWSE", icons::FOLDER))
                                                        .compact(),
                                                )
                                                .on_hover_text("Open installation folder")
                                                .clicked();
                                            if clicked {
                                                click(LibraryAction::OpenFolder(dir.clone()));
                                            }
                                        }
                                        if ui
                                            .add(ghost_button(&format!("{}  STORE", icons::STORE)).compact())
                                            .on_hover_text("Open Store page in browser")
                                            .clicked()
                                        {
                                            click(LibraryAction::Details(entry.app_id));
                                        }
                                        if ui
                                            .add(ghost_button(&format!("{}  VERIFY", icons::CHECK)).compact())
                                            .on_hover_text("Verify downloaded files")
                                            .clicked()
                                        {
                                            click(LibraryAction::VerifyDrydock(entry.app_id));
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  UPDATE", icons::UPDATES))
                                                    .compact(),
                                            )
                                            .on_hover_text("Check for updated files")
                                            .clicked()
                                        {
                                            click(LibraryAction::UpdateDrydock(entry.app_id));
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  CRACK", icons::SPARKLES))
                                                    .compact(),
                                            )
                                            .on_hover_text("Deploy emu crack into game folder")
                                            .clicked()
                                        {
                                            click(LibraryAction::CrackDrydock(entry.app_id));
                                        }
                                        if entry.launch_path.is_some() {
                                            let clicked = ui
                                                .add(
                                                    ghost_button(&format!(
                                                        "{}  CHANGE .EXE",
                                                        icons::SETTINGS
                                                    ))
                                                    .compact(),
                                                )
                                                .on_hover_text("Choose a different launch executable")
                                                .clicked();
                                            if clicked {
                                                click(LibraryAction::SetExe(entry.app_id));
                                            }
                                        }
                                        if ui
                                            .add(
                                                ghost_button(&format!("{}  UNINSTALL", icons::CLOSE))
                                                    .compact(),
                                            )
                                            .on_hover_text("Delete the downloaded game folder")
                                            .clicked()
                                        {
                                            click(LibraryAction::UninstallDrydock(entry.app_id));
                                        }
                                    }
                                }
                            });
                        });

                    ui.add_space(16.0);

                    // ── Info divider ─────────────────────────────────────────
                    ui.painter().line_segment(
                        [
                            egui::pos2(ui.min_rect().left(), ui.cursor().min.y),
                            egui::pos2(ui.min_rect().left() + body_w, ui.cursor().min.y),
                        ],
                        Stroke::new(1.0, BORDER),
                    );
                    ui.add_space(12.0);

                    // ── Key-value info rows ──────────────────────────────────
                    let label_color = MUTED;
                    let value_color = TEXT;
                    let row_font = egui::FontId::proportional(13.5);

                    let info_row = |ui: &mut egui::Ui, label: &str, value: &str, vcolor: Color32| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label(RichText::new(label).font(row_font.clone()).color(label_color));
                            ui.label(RichText::new(value).font(row_font.clone()).color(vcolor));
                        });
                        ui.add_space(5.0);
                    };

                    // Platform
                    info_row(ui, "Platform:  ", "Windows (x64)", value_color);

                    // Executable
                    if let Some(path) = &entry.launch_path {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Executable:  ")
                                    .font(row_font.clone())
                                    .color(label_color),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(path).font(row_font.clone()).color(value_color),
                                )
                                .truncate(),
                            )
                            .on_hover_text(path);
                        });
                        ui.add_space(5.0);
                    } else if entry.source == LibrarySource::SteamInstalled {
                        info_row(
                            ui,
                            "Executable:  ",
                            &format!("steam://run/{}", entry.app_id),
                            value_color,
                        );
                    } else {
                        info_row(ui, "Executable:  ", "No launcher linked", AMBER);
                    }

                    // Size on disk
                    if let Some(size) = entry.size_on_disk {
                        info_row(ui, "Size on disk:  ", &human_bytes(size), value_color);
                    }

                    // Install directory
                    if let Some(dir) = &entry.install_dir {
                        let dir_str = dir.display().to_string();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Location:  ")
                                    .font(row_font.clone())
                                    .color(label_color),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&dir_str).font(row_font.clone()).color(value_color),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&dir_str);
                        });
                        ui.add_space(5.0);
                        ui.add_space(4.0);
                        if ui
                            .add(ghost_button(&format!("{}  Open Folder", icons::FOLDER)).compact())
                            .clicked()
                        {
                            action = Some(LibraryAction::OpenFolder(dir.clone()));
                        }
                    } else {
                        info_row(ui, "Location:  ", "Not installed on local disk", MUTED);
                    }

                    ui.add_space(4.0);
                });
        });

    action
}

impl DrydockApp {
    pub fn library_page(&mut self, ui: &mut egui::Ui) {
        // When a folder has been chosen for "Add game to Drydock", the page becomes the add-mode picker.
        if self.add_game_folder.is_some() {
            self.render_add_game_panel(ui);
            return;
        }
        // Installed (real) games first, then games activated in Drydock that Steam hasn't installed.
        let mut entries: Vec<LibraryEntry> = Vec::new();
        let mut seen: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for manifest in &self.manifests {
            if is_real_game(manifest.app_id, &manifest.name) && seen.insert(manifest.app_id) {
                entries.push(LibraryEntry {
                    app_id: manifest.app_id,
                    name: manifest.name.clone(),
                    installed: true,
                    launch_path: self.settings.launch_paths.get(&manifest.app_id).cloned(),
                    install_dir: Some(manifest.install_dir()),
                    size_on_disk: manifest.size_on_disk,
                    source: LibrarySource::SteamInstalled,
                });
            }
        }
        // Games Drydock downloaded through its own depot engine into the games folder (not the Steam
        // library) — shown alongside the Steam-detected ones.
        for (app_id, game) in &self.settings.installed_games {
            if seen.insert(*app_id) {
                let dir = PathBuf::from(&game.install_dir);
                entries.push(LibraryEntry {
                    app_id: *app_id,
                    name: game.name.clone(),
                    installed: true,
                    launch_path: self.settings.launch_paths.get(app_id).cloned(),
                    install_dir: if dir.is_dir() { Some(dir) } else { None },
                    size_on_disk: None,
                    source: LibrarySource::DrydockInstalled,
                });
            }
        }
        // Apps that have an unlock Lua but no install: the union of what is actually in
        // `config/stplug-in` and what this installation recorded.
        let available: std::collections::BTreeSet<u32> = self
            .plugin_luas
            .iter()
            .copied()
            .chain(self.settings.added_apps.keys().copied())
            .collect();
        for app_id in available {
            if !seen.insert(app_id) {
                continue;
            }
            let name = self
                .catalog
                .iter()
                .find(|entry| entry.app_id == app_id)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| format!("App {app_id}"));
            if !is_real_game(app_id, &name) {
                continue;
            }
            entries.push(LibraryEntry {
                app_id,
                name,
                installed: false,
                launch_path: self.settings.launch_paths.get(&app_id).cloned(),
                install_dir: None,
                size_on_disk: None,
                source: LibrarySource::Available,
            });
        }
        entries.sort_by_key(|entry| entry.name.to_lowercase());

        // Set by the "Add game to Drydock" button; the folder picker runs after the borrow of `entries`
        // and the UI closures ends (a native dialog can't open mid-layout).
        let mut add_game_requested = false;

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  Library", icons::LIBRARY));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let installed_cnt = entries.iter().filter(|e| e.installed).count();
                let avail_cnt = entries.len() - installed_cnt;
                ui.spacing_mut().item_spacing.x = 6.0;
                status_pill(ui, &format!("{} INSTALLED", installed_cnt), VERDIGRIS);
                status_pill(ui, &format!("{} AVAILABLE", avail_cnt), AMBER);
                status_pill(ui, &format!("{} TOTAL", entries.len()), ACCENT);
            });
        });

        ui.add_space(10.0);

        // Top Toolbar: Search + Filter Tabs + Add Game Button
        let toolbar_h = 36.0;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), toolbar_h),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);

                // 1. Sleek Integrated Search Bar (exact 36px height, vertically centered)
                let search_w = 220.0;
                ui.allocate_ui_with_layout(
                    Vec2::new(search_w, toolbar_h),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_height(toolbar_h);
                        ui.set_width(search_w);
                        let rect = ui.max_rect();
                        let search_id = ui.id().with("library_search_input");
                        let is_focused = ui.ctx().memory(|m| m.has_focus(search_id));
                        let stroke_color = if is_focused { ACCENT } else { BORDER };

                        ui.painter().rect(
                            rect,
                            8.0,
                            SURFACE_RAISED,
                            Stroke::new(1.0, stroke_color),
                            egui::StrokeKind::Inside,
                        );

                        ui.add_space(10.0);
                        ui.label(RichText::new(icons::SEARCH).size(14.0).color(if is_focused {
                            ACCENT
                        } else {
                            MUTED
                        }));
                        ui.add_space(4.0);
                        let text_edit = egui::TextEdit::singleline(&mut self.library_search)
                            .id(search_id)
                            .hint_text(RichText::new("Filter library...").size(14.0).color(MUTED))
                            .frame(egui::Frame::NONE)
                            .desired_width(search_w - 56.0);
                        ui.add(text_edit);
                        if !self.library_search.is_empty()
                            && ui
                                .add(
                                    egui::Button::new(RichText::new(icons::CLOSE).size(12.0).color(MUTED))
                                        .frame(false),
                                )
                                .clicked()
                        {
                            self.library_search.clear();
                        }
                    },
                );

                // 2. Filter Pills (exact 36px height, matching 8px radius)
                let installed_cnt = entries.iter().filter(|e| e.installed).count();
                let avail_cnt = entries.len().saturating_sub(installed_cnt);
                let filters = [
                    (LibraryFilter::All, format!("All ({})", entries.len())),
                    (LibraryFilter::Installed, format!("Installed ({installed_cnt})")),
                    (LibraryFilter::Available, format!("Available ({avail_cnt})")),
                ];

                for (filter, label) in filters {
                    let is_active = self.library_filter == filter;
                    let (bg, stroke, text_color) = if is_active {
                        (ACCENT, Stroke::NONE, Color32::from_rgb(4, 14, 24))
                    } else {
                        (SURFACE_RAISED, Stroke::new(1.0, BORDER), MUTED)
                    };

                    let btn = egui::Button::new(RichText::new(label).size(13.5).strong().color(text_color))
                        .fill(bg)
                        .stroke(stroke)
                        .corner_radius(8)
                        .min_size(Vec2::new(0.0, toolbar_h));

                    if ui.add(btn).clicked() {
                        self.library_filter = filter;
                    }
                }

                // 3. Add Game Button (right-aligned, exact 36px height, matching 8px radius)
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let btn = egui::Button::new(
                        RichText::new(format!("{}  ADD LOCAL GAME", icons::FOLDER))
                            .size(13.5)
                            .strong()
                            .color(MUTED),
                    )
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(8)
                    .min_size(Vec2::new(155.0, toolbar_h));

                    if ui.add(btn).clicked() {
                        add_game_requested = true;
                    }
                });
            },
        );

        ui.add_space(14.0);

        if entries.is_empty() {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(16)
                .inner_margin(32)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(16.0);
                        ui.label(RichText::new(icons::LIBRARY).size(36.0).color(ACCENT));
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("Your Library is Empty")
                                .size(20.0)
                                .strong()
                                .color(TEXT),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Games installed through Steam or activated in Drydock will appear here.",
                            )
                            .size(14.0)
                            .color(MUTED),
                        );
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(12.0, 0.0);
                            if ui
                                .add(
                                    primary_button(&format!("{}  BROWSE STORE", icons::STORE))
                                        .min_size(Vec2::new(160.0, 42.0)),
                                )
                                .clicked()
                            {
                                self.page = Page::Home;
                            }
                            if ui
                                .add(
                                    ghost_button(&format!("{}  ADD LOCAL GAME", icons::FOLDER))
                                        .min_size(Vec2::new(160.0, 42.0)),
                                )
                                .clicked()
                            {
                                add_game_requested = true;
                            }
                        });
                        ui.add_space(16.0);
                    });
                });
            if add_game_requested {
                self.begin_add_game();
            }
            return;
        }

        // Filter games based on current filter & search query
        let search_term = self.library_search.trim().to_lowercase();
        let filtered: Vec<&LibraryEntry> = entries
            .iter()
            .filter(|e| match self.library_filter {
                LibraryFilter::All => true,
                LibraryFilter::Installed => e.installed,
                LibraryFilter::Available => !e.installed,
            })
            .filter(|e| {
                if search_term.is_empty() {
                    true
                } else {
                    e.name.to_lowercase().contains(&search_term)
                }
            })
            .collect();

        let mut action: Option<LibraryAction> = None;

        if filtered.is_empty() {
            ui.add_space(36.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No games match your search or filter.")
                        .size(16.0)
                        .color(MUTED),
                );
                ui.add_space(10.0);
                if ui.add(ghost_button("Clear filters")).clicked() {
                    self.library_search.clear();
                    self.library_filter = LibraryFilter::All;
                }
            });
        } else {
            // Responsive Grid of Portrait Poster Cards (2:3 aspect ratio)
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(16.0, 18.0);
                for entry in filtered {
                    if library_poster_card(ui, entry) {
                        self.library_modal = Some(entry.app_id);
                        self.library_selected = Some(entry.app_id);
                    }
                }
            });
        }

        // ── Game Details Modal Overlay Window ───────────────────────────────────────────
        if let Some(modal_app_id) = self.library_modal {
            if let Some(entry) = entries.iter().find(|e| e.app_id == modal_app_id) {
                let screen = ui.ctx().content_rect();
                let mut close_requested = false;

                // 1. Escape key closes modal
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close_requested = true;
                }

                // 2. Full-window backdrop dimmer (covers entire window: sidebar, status bar, and central content)
                let backdrop_id = egui::Id::new("library_modal_backdrop");
                egui::Area::new(backdrop_id)
                    .order(egui::Order::Middle)
                    .fixed_pos(screen.min)
                    .interactable(true)
                    .show(ui.ctx(), |ui| {
                        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                        ui.painter()
                            .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 185));
                        if resp.clicked() {
                            close_requested = true;
                        }
                    });

                // 3. Centered modal dialog window (rendered after backdrop on Order::Middle so it stays on top)
                let modal_w = 580.0_f32.min(screen.width() - 40.0);
                let modal_h = 560.0_f32.min(screen.height() - 60.0);

                egui::Window::new("library_game_modal_dialog")
                    .title_bar(false)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .fixed_size(Vec2::new(modal_w, modal_h))
                    .frame(
                        egui::Frame::new()
                            .fill(SURFACE)
                            .stroke(Stroke::new(1.5, lerp_color(BORDER, ACCENT, 0.4)))
                            .corner_radius(16)
                            .inner_margin(0),
                    )
                    .show(ui.ctx(), |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        if let Some(overview_action) =
                            library_overview(ui, entry, &self.catalog, modal_w, modal_h, &mut close_requested)
                        {
                            action = Some(overview_action);
                        }
                    });

                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close_requested = true;
                }

                if close_requested {
                    self.library_modal = None;
                }
            } else {
                self.library_modal = None;
            }
        }

        match action {
            Some(LibraryAction::Select(app_id)) => {
                self.library_selected = Some(app_id);
                self.library_modal = Some(app_id);
            }
            Some(LibraryAction::Details(app_id)) => self.open_details(app_id),
            Some(LibraryAction::Launch(app_id)) => self.launch_library_game(app_id),
            Some(LibraryAction::SetExe(app_id)) => self.set_library_launch_path(app_id),
            Some(LibraryAction::InstallSteam(app_id)) => self.install_steam_game(app_id),
            Some(LibraryAction::UninstallSteam(app_id)) => self.uninstall_steam_game(app_id),
            Some(LibraryAction::UpdateLua(app_id)) => self.add_app_to_steam(app_id),
            Some(LibraryAction::RemoveLua(app_id)) => {
                self.remove_lua_confirmed(app_id);
                self.library_modal = None;
            }
            Some(LibraryAction::VerifyDrydock(app_id)) => {
                let name = self.app_display_name(app_id);
                self.start_verify(app_id, name);
            }
            Some(LibraryAction::UpdateDrydock(app_id)) => {
                let name = self.app_display_name(app_id);
                self.enqueue_download(app_id, name);
                self.status = format!("Checking {} for updates…", self.app_display_name(app_id));
                self.status_error = false;
            }
            Some(LibraryAction::CrackDrydock(app_id)) => self.crack_drydock_game(app_id),
            Some(LibraryAction::UninstallDrydock(app_id)) => {
                self.uninstall_drydock_game(app_id);
                self.library_modal = None;
            }
            Some(LibraryAction::OpenFolder(path)) => {
                if let Err(error) = open::that(&path) {
                    self.status = format!("Could not open folder: {error}");
                    self.status_error = true;
                }
            }
            None => {}
        }

        if add_game_requested {
            self.begin_add_game();
        }
    }

    /// Launches a library game: a stored `.exe` (for games activated outside Steam) wins; otherwise
    /// Steam is asked to run the App ID.
    pub fn launch_library_game(&mut self, app_id: u32) {
        if let Some(path) = self.settings.launch_paths.get(&app_id).cloned() {
            let exe = PathBuf::from(&path);
            let mut command = Command::new(&exe);
            if let Some(parent) = exe.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                command.current_dir(parent);
            }
            match command.spawn() {
                Ok(_) => {
                    self.status = "Launching the game".into();
                    self.status_error = false;
                }
                Err(error) => {
                    self.status = format!("The game could not be launched: {error}");
                    self.status_error = true;
                }
            }
            return;
        }
        match open_steam_uri(app_id, SteamUriAction::Run) {
            Ok(()) => {
                self.status = "Asking Steam to launch the game".into();
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// Picks the launch `.exe` for a game activated outside Steam, and remembers it so the Play
    /// button starts it directly next time.
    pub fn set_library_launch_path(&mut self, app_id: u32) {
        let mut dialog = rfd::FileDialog::new().set_title("Select the game's .exe");
        if let Some(current) = self
            .settings
            .launch_paths
            .get(&app_id)
            .map(PathBuf::from)
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .filter(|parent| parent.is_dir())
        {
            dialog = dialog.set_directory(current);
        }
        #[cfg(windows)]
        {
            dialog = dialog.add_filter("Executable", &["exe"]);
        }
        if let Some(file) = dialog.pick_file() {
            self.settings
                .launch_paths
                .insert(app_id, file.display().to_string());
            self.status_error = self.persist_settings().is_err();
            if self.status_error {
                self.status = "The launch path could not be saved".into();
            } else {
                self.status = "Launch path saved".into();
            }
        }
    }

    /// Asks Steam to install a game whose unlock Lua is already in place (`steam://install`).
    /// Restores the app's Lua and depot manifests, then asks Steam to install it.
    ///
    /// The restore has to happen first, and it has to happen every time: Steam deletes an app's
    /// manifests from `depotcache` when it is uninstalled, and putting them back afterwards is too
    /// late — Steam has already decided to fetch them itself. Restoring from the local store keeps a
    /// reinstall instant and works with no network at all.
    pub fn install_steam_game(&mut self, app_id: u32) {
        let restored = match self.steam.root.clone() {
            Some(root) => restore_payload_into_steam(&self.payload_store, &root, app_id),
            None => Ok((false, 0)),
        };
        // A failed restore is worth saying out loud but must not block the install: Steam can still
        // fetch what it needs, it is just slower.
        let prefix = match &restored {
            Ok((_, 0)) => String::new(),
            Ok((lua, manifests)) => {
                let lua = if *lua { "unlock and " } else { "" };
                format!("Restored the {lua}{manifests} depot manifest(s). ")
            }
            Err(error) => {
                self.status_error = true;
                format!("The stored files could not be restored ({error}). ")
            }
        };
        match open_steam_uri(app_id, SteamUriAction::Install) {
            Ok(()) => {
                self.status = format!("{prefix}Asking Steam to install the game");
                self.status_error = restored.is_err();
            }
            Err(error) => {
                self.status = format!("{prefix}{error}");
                self.status_error = true;
            }
        }
    }

    /// Asks Steam to uninstall a Steam-installed game (`steam://uninstall`); Steam shows its own
    /// confirmation, so no extra dialog here.
    pub fn uninstall_steam_game(&mut self, app_id: u32) {
        match open_steam_uri(app_id, SteamUriAction::Uninstall) {
            Ok(()) => {
                self.status = "Asking Steam to uninstall the game".into();
                self.status_error = false;
            }
            Err(error) => {
                self.status = error.to_string();
                self.status_error = true;
            }
        }
    }

    /// Removes a game's unlock Lua from Steam after a confirmation prompt.
    pub fn remove_lua_confirmed(&mut self, app_id: u32) {
        let name = self.app_display_name(app_id);
        let confirmed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Remove unlock Lua")
            .set_description(format!(
                "Delete the unlock Lua for \"{name}\" from Steam? You can add it again any time."
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed == rfd::MessageDialogResult::Yes {
            self.remove_app_from_steam(app_id);
        }
    }

    /// Deletes a Drydock-downloaded game's install folder (after confirmation) and forgets it.
    pub fn uninstall_drydock_game(&mut self, app_id: u32) {
        let Some(game) = self.settings.installed_games.get(&app_id).cloned() else {
            return;
        };
        let name = if game.name.is_empty() {
            self.app_display_name(app_id)
        } else {
            game.name.clone()
        };
        let confirmed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Uninstall game")
            .set_description(format!(
                "Delete \"{name}\" and all of its downloaded files from:\n{}\n\nThis cannot be undone.",
                game.install_dir
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed != rfd::MessageDialogResult::Yes {
            return;
        }
        let dir = PathBuf::from(&game.install_dir);
        if dir.is_dir()
            && let Err(error) = std::fs::remove_dir_all(&dir)
        {
            self.status = format!("Could not delete the game folder: {error}");
            self.status_error = true;
            return;
        }
        self.settings.installed_games.remove(&app_id);
        self.settings
            .download_queue
            .retain(|queued| queued.app_id != app_id);
        self.status_error = self.persist_settings().is_err();
        self.status = if self.status_error {
            format!("\"{name}\" was deleted, but the change could not be saved")
        } else {
            format!("\"{name}\" was uninstalled")
        };
    }

    /// Runs the emu crack flow for a Drydock-downloaded game, deploying it straight into the game's
    /// install folder. Reuses the same generator as the Tools cracker (and the Tools panel's arch /
    /// loader / REFramework choices), just with the output fixed to this game's folder.
    pub fn begin_add_game(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Select the game's folder")
            .pick_folder()
        {
            self.add_game_folder = Some(folder);
            self.add_game_search.clear();
        }
    }

    /// Detects a chosen folder in the background: it fetches the game's launch executables from Steam,
    /// resolves the real game root inside the folder, and finds the launch `.exe` — exactly as the
    /// activation folder check does — so the game can be added to the Drydock library and played.
    pub fn start_add_game_detect(&mut self, app_id: u32, folder: PathBuf) {
        if self.add_game_receiver.is_some() {
            return;
        }
        let name = self.app_display_name(app_id);
        let (sender, receiver) = mpsc::channel();
        self.add_game_receiver = Some(receiver);
        self.busy_label = Some(format!("Detecting {name}…"));
        std::thread::spawn(move || {
            let result = (|| -> Result<AddGameOutcome, String> {
                if !folder.is_dir() {
                    return Err("Select the game's folder first.".to_owned());
                }
                let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
                if executables.is_empty() {
                    return Err("Steam lists no launch executable for this game.".to_owned());
                }
                let root = resolve_game_root(&folder, &executables).ok_or_else(|| {
                    "These files don't look like the selected game. Pick the correct game folder.".to_owned()
                })?;
                // resolve_game_root guarantees at least one executable exists under the root; use the
                // first one that does as the Play launcher.
                let exe = executables
                    .iter()
                    .map(|relative| root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
                    .find(|path| path.is_file())
                    .ok_or_else(|| "The launch executable could not be found in the folder.".to_owned())?;
                Ok(AddGameOutcome {
                    app_id,
                    name,
                    root,
                    exe,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Applies a finished "Add game to Drydock" detection: records the game under the Drydock library and
    /// remembers its launch `.exe` so Play works.
    pub fn poll_add_game(&mut self) {
        let Some(receiver) = self.add_game_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.add_game_receiver = None;
                self.busy_label = None;
                match result {
                    Ok(outcome) => {
                        self.settings.installed_games.insert(
                            outcome.app_id,
                            drydock_core::InstalledGame {
                                name: outcome.name.clone(),
                                install_dir: outcome.root.display().to_string(),
                            },
                        );
                        self.settings
                            .launch_paths
                            .insert(outcome.app_id, outcome.exe.display().to_string());
                        self.status_error = self.persist_settings().is_err();
                        self.status = if self.status_error {
                            format!(
                                "\"{}\" was added, but the change could not be saved",
                                outcome.name
                            )
                        } else {
                            format!("\"{}\" was added to your Drydock library", outcome.name)
                        };
                        self.add_game_folder = None;
                        self.add_game_search.clear();
                        self.library_selected = Some(outcome.app_id);
                    }
                    Err(error) => {
                        self.status = error;
                        self.status_error = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.add_game_receiver = None,
        }
    }

    /// After a depot download finishes, detects the game's install folder and launch `.exe` in the
    /// background (the same install root the depot engine wrote to, plus Steam's executable list) so
    /// the game registers itself under "Installed in Drydock" with a working Play button.
    pub fn start_download_install_detect(&mut self, app_id: u32, name: String) {
        if self.download_install_receiver.is_some() {
            return;
        }
        let steam_root = self.steam.root.clone();
        let installed_dir = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(SteamManifest::install_dir);
        let (sender, receiver) = mpsc::channel();
        self.download_install_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| -> Result<AddGameOutcome, String> {
                // The same install root the depot engine wrote to (mirrors run_depot_job).
                let install_root = match installed_dir {
                    Some(dir) => dir,
                    None => {
                        let root = steam_root.ok_or_else(|| "Steam folder not found.".to_owned())?;
                        let installdir = fetch_install_dir(app_id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| {
                                "Steam did not report an install folder for this game.".to_owned()
                            })?;
                        root.join("steamapps").join("common").join(installdir)
                    }
                };
                let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
                // The depot writes straight into the install root; resolve a nested root only if the
                // launch exe lives in a subfolder.
                let root = resolve_game_root(&install_root, &executables).unwrap_or(install_root);
                let exe = executables
                    .iter()
                    .map(|relative| root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
                    .find(|path| path.is_file())
                    .unwrap_or_default();
                Ok(AddGameOutcome {
                    app_id,
                    name,
                    root,
                    exe,
                })
            })();
            let _ = sender.send(result);
        });
    }

    /// Registers a finished depot download in the Drydock library (quietly — the download's own success
    /// message stays on screen). A detection failure is ignored: the files are still on disk and the
    /// game can be added manually.
    pub fn poll_download_install(&mut self) {
        let Some(receiver) = self.download_install_receiver.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.download_install_receiver = None;
                if let Ok(outcome) = result {
                    self.settings.installed_games.insert(
                        outcome.app_id,
                        drydock_core::InstalledGame {
                            name: outcome.name.clone(),
                            install_dir: outcome.root.display().to_string(),
                        },
                    );
                    if !outcome.exe.as_os_str().is_empty() {
                        self.settings
                            .launch_paths
                            .insert(outcome.app_id, outcome.exe.display().to_string());
                    }
                    let _ = self.persist_settings();
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.download_install_receiver = None,
        }
    }

    /// The add-mode panel shown in place of the Library while a folder is chosen: the picked folder
    /// plus a game picker to say which game it is (then background detection runs).
    pub fn render_add_game_panel(&mut self, ui: &mut egui::Ui) {
        let Some(folder) = self.add_game_folder.clone() else {
            return;
        };
        ui.add_space(16.0);
        page_heading(ui, &format!("{}  Add Game to Drydock", icons::FOLDER));
        ui.add_space(4.0);
        ui.add(egui::Label::new(
            RichText::new("Point Drydock at a game folder on disk — it detects the install root and launch executable so you can launch and manage it from your library.")
                .size(14.0)
                .color(MUTED),
        ));
        ui.add_space(16.0);

        let busy = self.add_game_receiver.is_some();
        let mut picked: Option<u32> = None;
        let mut cancel = false;

        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                // Step 1: Selected Folder Card
                egui::Frame::new()
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(12)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}  STEP 1: SELECTED GAME DIRECTORY", icons::FOLDER))
                                    .size(14.0)
                                    .strong()
                                    .color(ACCENT),
                            );
                        });
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(folder.display().to_string()).size(14.0).color(TEXT),
                            )
                            .wrap(),
                        );
                    });

                ui.add_space(16.0);

                // Step 2: Match Game Title Card
                egui::Frame::new()
                    .fill(SURFACE_RAISED)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(12)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}  STEP 2: IDENTIFY GAME IN CATALOG", icons::SEARCH))
                                    .size(14.0)
                                    .strong()
                                    .color(ACCENT),
                            );
                        });
                        ui.add_space(6.0);

                        if busy {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("Analyzing files and detecting launch executable…")
                                        .size(14.0)
                                        .color(MUTED),
                                );
                            });
                        } else {
                            ui.label(
                                RichText::new(
                                    "Type the game's name to link its Steam App ID and launch manifests:",
                                )
                                .size(14.0)
                                .color(MUTED),
                            );
                            ui.add_space(8.0);
                            let width = ui.available_width();
                            picked = game_search_box(
                                ui,
                                "add_game_pick",
                                &mut self.add_game_search,
                                None,
                                &self.catalog,
                                &self.header_resolver,
                                width,
                                true,
                                |_| true,
                            );
                        }
                    });

                ui.add_space(20.0);
                if ui
                    .add(ghost_button("CANCEL & RETURN").min_size(Vec2::new(160.0, 40.0)))
                    .clicked()
                {
                    cancel = true;
                }
            });

        if cancel {
            self.add_game_folder = None;
            self.add_game_search.clear();
        } else if let Some(app_id) = picked {
            self.start_add_game_detect(app_id, folder);
        }
    }
}
