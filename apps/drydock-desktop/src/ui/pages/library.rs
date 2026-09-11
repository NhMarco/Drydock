use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};

use drydock_core::*;
use eframe::egui::{self, Align, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;

/// Height of one game row in the Library rail.
pub const RAIL_ROW_H: f32 = 44.0;

/// One collapsible Library rail group (e.g. "Installed in Steam"): a header with the game count and,
/// when expanded, the group's rows. Always rendered (even empty), open by default. Returns the App ID
/// of a row the user clicked, if any.
pub fn library_rail_group(
    ui: &mut egui::Ui,
    title: &str,
    source: LibrarySource,
    entries: &[LibraryEntry],
    selected: Option<u32>,
) -> Option<u32> {
    let group: Vec<&LibraryEntry> = entries.iter().filter(|entry| entry.source == source).collect();
    let mut clicked = None;
    let header = format!("{title}  ({})", group.len());
    egui::CollapsingHeader::new(RichText::new(header).size(12.0).strong().color(TEXT))
        .id_salt(("library_group", title))
        .default_open(true)
        .show(ui, |ui| {
            if group.is_empty() {
                ui.add_space(2.0);
                ui.label(RichText::new("No games here yet.").size(11.0).color(MUTED));
                ui.add_space(2.0);
                return;
            }
            for entry in group {
                if library_rail_row(ui, entry, selected == Some(entry.app_id)) {
                    clicked = Some(entry.app_id);
                }
            }
        });
    ui.add_space(4.0);
    clicked
}

/// One entry in the Steam-style Library rail: a small landscape thumbnail + title. Games that exist
/// only through Drydock' lua/manifest activation (not installed by Steam) are dimmed grey; the
/// highlighted game gets a violet fill and accent bar. Returns true when clicked.
pub fn library_rail_row(ui: &mut egui::Ui, entry: &LibraryEntry, selected: bool) -> bool {
    let dim = !entry.installed;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), RAIL_ROW_H), Sense::click());
    let hover = ui.ctx().animate_bool(resp.id, resp.hovered());
    let fill = if selected {
        lerp_color(SIDEBAR_FILL, ACCENT_DEEP, 0.40)
    } else {
        lerp_color(SIDEBAR_FILL, SURFACE_RAISED, hover)
    };
    ui.painter().rect_filled(rect, egui::CornerRadius::same(6), fill);
    if selected {
        let bar = egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
        ui.painter().rect_filled(bar, egui::CornerRadius::same(2), ACCENT);
    }
    let th = RAIL_ROW_H - 16.0;
    let tw = th / STEAM_HEADER_ASPECT;
    let thumb = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 9.0, rect.center().y - th / 2.0),
        Vec2::new(tw, th),
    );
    let urls = steam_artwork_urls(entry.app_id);
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    // Cover-fit so the thumbnail fills its rect with no letterbox bars.
    paint_remote_image_cover_multi(ui, thumb, &refs, egui::CornerRadius::same(3));
    let color = if dim && !selected { MUTED } else { TEXT };
    ui.painter().with_clip_rect(rect).text(
        egui::pos2(thumb.right() + 11.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &entry.name,
        FontId::proportional(13.0),
        color,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// The right-hand Library overview of the highlighted game: a full-width header banner, the title,
/// an Installed/Activated status line, and the Play / Set-.exe / Store-page controls.
pub fn library_overview(ui: &mut egui::Ui, entry: &LibraryEntry, body_h: f32) -> Option<LibraryAction> {
    let mut action = None;
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(14)
        .inner_margin(0)
        .show(ui, |ui| {
            ui.set_min_height(body_h - 2.0);
            let width = ui.available_width();
            let hero_h = (width * STEAM_HEADER_ASPECT).min(body_h * 0.5);
            let (hero, _) = ui.allocate_exact_size(Vec2::new(width, hero_h), Sense::hover());
            // Prefer the wide `library_hero.jpg` (built for banners), then the header/capsules, and
            // cover-fit (fill + crop) so the art fills the banner edge to edge — no letterbox bars,
            // whatever the source image's aspect ratio.
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
            paint_remote_image_cover_multi(
                ui,
                hero,
                &refs,
                egui::CornerRadius {
                    nw: 14,
                    ne: 14,
                    sw: 0,
                    se: 0,
                },
            );
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(22, 18))
                .show(ui, |ui| {
                    ui.label(RichText::new(&entry.name).size(22.0).strong().color(TEXT));
                    ui.add_space(8.0);
                    let amber = AMBER;
                    let (status, color) = match entry.source {
                        LibrarySource::SteamInstalled => ("Installed in Steam — ready to play", VERDIGRIS),
                        LibrarySource::DrydockInstalled if entry.launch_path.is_some() => {
                            ("Installed by Drydock · launches from a linked .exe", ACCENT_SOFT)
                        }
                        LibrarySource::DrydockInstalled => {
                            ("Installed by Drydock · link a launcher to play", amber)
                        }
                        LibrarySource::Available => ("Lua ready in Steam · not installed yet", amber),
                    };
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, color);
                        ui.add_space(5.0);
                        ui.label(RichText::new(status).size(12.5).color(color));
                    });
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new(format!("App ID {}", entry.app_id))
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(20.0);
                    let bh = 42.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        let mut click = |a: LibraryAction| action = Some(a);
                        match entry.source {
                            LibrarySource::Available => {
                                if ui
                                    .add(primary_button("INSTALL").min_size(Vec2::new(150.0, bh)))
                                    .clicked()
                                {
                                    click(LibraryAction::InstallSteam(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UPDATE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Re-fetch and re-install the unlock Lua")
                                    .clicked()
                                {
                                    click(LibraryAction::UpdateLua(entry.app_id));
                                }
                            }
                            LibrarySource::SteamInstalled => {
                                if ui
                                    .add(success_button("▶  PLAY").min_size(Vec2::new(140.0, bh)))
                                    .clicked()
                                {
                                    click(LibraryAction::Launch(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UPDATE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Re-fetch and re-install the unlock Lua")
                                    .clicked()
                                {
                                    click(LibraryAction::UpdateLua(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UNINSTALL").min_size(Vec2::new(120.0, bh)))
                                    .on_hover_text("Ask Steam to uninstall the game")
                                    .clicked()
                                {
                                    click(LibraryAction::UninstallSteam(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("REMOVE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Delete the unlock Lua from Steam")
                                    .clicked()
                                {
                                    click(LibraryAction::RemoveLua(entry.app_id));
                                }
                            }
                            LibrarySource::DrydockInstalled => {
                                if entry.launch_path.is_some() {
                                    if ui
                                        .add(success_button("▶  PLAY").min_size(Vec2::new(140.0, bh)))
                                        .clicked()
                                    {
                                        click(LibraryAction::Launch(entry.app_id));
                                    }
                                } else if ui
                                    .add(primary_button("SET .EXE").min_size(Vec2::new(140.0, bh)))
                                    .on_hover_text("Link the game's .exe so PLAY can launch it")
                                    .clicked()
                                {
                                    click(LibraryAction::SetExe(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("VERIFY").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Verify the downloaded files against the depot manifests")
                                    .clicked()
                                {
                                    click(LibraryAction::VerifyDrydock(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UPDATE").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text("Check the depot for updated files and download them")
                                    .clicked()
                                {
                                    click(LibraryAction::UpdateDrydock(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("CRACK").min_size(Vec2::new(110.0, bh)))
                                    .on_hover_text(
                                        "Generate and deploy the emu crack into this game's folder",
                                    )
                                    .clicked()
                                {
                                    click(LibraryAction::CrackDrydock(entry.app_id));
                                }
                                if ui
                                    .add(ghost_button("UNINSTALL").min_size(Vec2::new(120.0, bh)))
                                    .on_hover_text("Delete the downloaded game folder")
                                    .clicked()
                                {
                                    click(LibraryAction::UninstallDrydock(entry.app_id));
                                }
                            }
                        }
                        if ui
                            .add(ghost_button("STORE PAGE").min_size(Vec2::new(130.0, bh)))
                            .clicked()
                        {
                            click(LibraryAction::Details(entry.app_id));
                        }
                    });
                });
        });
    action
}

/// Formats an integer with thin thousands separators (e.g. 182140 -> "182,140").
/// Runs a depot download or verify to completion on a background thread, forwarding progress ticks
/// through `sender`. Returns a human summary on success or an error message.
/// Where a generated crack is written.

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
                    source: LibrarySource::SteamInstalled,
                });
            }
        }
        // Games Drydock downloaded through its own depot engine into the games folder (not the Steam
        // library) — shown alongside the Steam-detected ones.
        for (app_id, game) in &self.settings.installed_games {
            if seen.insert(*app_id) {
                entries.push(LibraryEntry {
                    app_id: *app_id,
                    name: game.name.clone(),
                    installed: true,
                    launch_path: self.settings.launch_paths.get(app_id).cloned(),
                    source: LibrarySource::DrydockInstalled,
                });
            }
        }
        // Apps that have an unlock Lua but no install: the union of what is actually in
        // `config/stplug-in` and what this installation recorded. Disk first — the record is lost
        // with the settings file, and these games would then silently vanish from the library even
        // though Steam is still reading their Lua.
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
                source: LibrarySource::Available,
            });
        }
        entries.sort_by_key(|entry| entry.name.to_lowercase());

        // Set by the "Add game to Drydock" button; the folder picker runs after the borrow of `entries`
        // and the UI closures ends (a native dialog can't open mid-layout).
        let mut add_game_requested = false;

        ui.add_space(20.0);
        page_heading(ui, "Library");
        ui.add_space(4.0);
        ui.add(egui::Label::new(
            RichText::new("Your installed and activated games — launch any of them straight from Drydock.")
                .size(12.5)
                .color(MUTED),
        ));
        ui.add_space(16.0);

        if entries.is_empty() {
            panel(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("No games in your library yet.")
                        .size(13.5)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Install a game through Steam or activate one in Drydock and it will show up here.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(primary_button("BROWSE THE STORE")).clicked() {
                        self.page = Page::Home;
                    }
                    if ui.add(ghost_button("＋  ADD GAME TO DRYDOCK")).clicked() {
                        add_game_requested = true;
                    }
                });
            });
            if add_game_requested {
                self.begin_add_game();
            }
            return;
        }

        // Keep the highlighted game valid: default to the first, and drop a stale selection.
        if self
            .library_selected
            .is_none_or(|id| !entries.iter().any(|entry| entry.app_id == id))
        {
            self.library_selected = entries.first().map(|entry| entry.app_id);
        }

        // A fixed-height body so the rail and the overview each get their own scroll — a nested
        // scroll area needs a definite height, which the growing outer page scroll doesn't provide.
        let top_used = TOP_NAV_HEIGHT + STATUS_BAR_HEIGHT + 18.0;
        let screen_h = ui
            .ctx()
            .input(|input| input.raw.screen_rect.map(|rect| rect.height()))
            .unwrap_or(900.0);
        let body_h = (screen_h - top_used - 150.0).max(340.0);
        let rail_w = 300.0_f32.min(ui.available_width() * 0.42);

        let mut action: Option<LibraryAction> = None;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), body_h),
            Layout::left_to_right(Align::Min),
            |ui| {
                // Left rail: every game, activated-only ones dimmed grey (Steam-style).
                ui.allocate_ui_with_layout(Vec2::new(rail_w, body_h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(rail_w);
                    egui::Frame::new()
                        .fill(SIDEBAR_FILL)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(12)
                        .inner_margin(6)
                        .show(ui, |ui| {
                            ui.set_min_height(body_h - 14.0);
                            let add_w = ui.available_width();
                            if ui
                                .add(ghost_button("＋  ADD GAME TO DRYDOCK").min_size(Vec2::new(add_w, 34.0)))
                                .clicked()
                            {
                                add_game_requested = true;
                            }
                            ui.add_space(6.0);
                            egui::ScrollArea::vertical()
                                .id_salt("library_rail")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let selected = self.library_selected;
                                    // Steam-style collapsible groups, all open by default.
                                    for (title, source) in [
                                        ("Installed in Steam", LibrarySource::SteamInstalled),
                                        ("Installed in Drydock", LibrarySource::DrydockInstalled),
                                        ("Available", LibrarySource::Available),
                                    ] {
                                        if let Some(id) =
                                            library_rail_group(ui, title, source, &entries, selected)
                                        {
                                            action = Some(LibraryAction::Select(id));
                                        }
                                    }
                                });
                        });
                });
                ui.add_space(20.0);
                // Right overview of the highlighted game. It needs its own top-down layout, or the
                // banner + info would flow horizontally inside this left-to-right row.
                if let Some(entry) = self
                    .library_selected
                    .and_then(|id| entries.iter().find(|entry| entry.app_id == id))
                {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), body_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            if let Some(overview_action) = library_overview(ui, entry, body_h) {
                                action = Some(overview_action);
                            }
                        },
                    );
                }
            },
        );

        match action {
            Some(LibraryAction::Select(app_id)) => self.library_selected = Some(app_id),
            Some(LibraryAction::Details(app_id)) => self.open_details(app_id),
            Some(LibraryAction::Launch(app_id)) => self.launch_library_game(app_id),
            Some(LibraryAction::SetExe(app_id)) => self.set_library_launch_path(app_id),
            Some(LibraryAction::InstallSteam(app_id)) => self.install_steam_game(app_id),
            Some(LibraryAction::UninstallSteam(app_id)) => self.uninstall_steam_game(app_id),
            Some(LibraryAction::UpdateLua(app_id)) => self.add_app_to_steam(app_id),
            Some(LibraryAction::RemoveLua(app_id)) => self.remove_lua_confirmed(app_id),
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
            Some(LibraryAction::UninstallDrydock(app_id)) => self.uninstall_drydock_game(app_id),
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
        ui.add_space(20.0);
        page_heading(ui, "Add a game to Drydock");
        ui.add_space(4.0);
        ui.add(egui::Label::new(
            RichText::new("Point Drydock at a game you already have on disk — it detects the folder and the launch .exe so you can play it from here.")
                .size(12.5)
                .color(MUTED),
        ));
        ui.add_space(16.0);

        let busy = self.add_game_receiver.is_some();
        let mut picked: Option<u32> = None;
        let mut cancel = false;
        panel(ui, |ui| {
            ui.label(RichText::new("GAME FOLDER").size(9.0).strong().color(ACCENT));
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(RichText::new(folder.display().to_string()).size(12.5).color(TEXT)).wrap(),
            );
            ui.add_space(16.0);
            ui.label(
                RichText::new("WHICH GAME IS THIS?")
                    .size(9.0)
                    .strong()
                    .color(ACCENT),
            );
            ui.add_space(6.0);
            if busy {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Detecting the game folder…")
                            .size(12.5)
                            .color(MUTED),
                    );
                });
            } else {
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
            ui.add_space(16.0);
            if ui
                .add(ghost_button("CANCEL").min_size(Vec2::new(120.0, 38.0)))
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

