use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::components::*;
use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

/// Whether a catalog app passes the active Home filters.
pub fn catalog_matches_filters(
    entry: &CatalogApp,
    repack_filter: &RepackFilter,
    fix_filter: FixFilter,
    repackers_by_app: &std::collections::HashMap<u32, Vec<String>>,
    fix_flags_by_app: &std::collections::HashSet<u32>,
) -> bool {
    let repack_ok = match repack_filter {
        RepackFilter::Any => true,
        RepackFilter::AnyRepack => repackers_by_app.contains_key(&entry.app_id),
        RepackFilter::Repacker(name) => {
            let name = name.to_lowercase();
            repackers_by_app
                .get(&entry.app_id)
                .is_some_and(|list| list.iter().any(|entry| entry == &name))
        }
    };
    if !repack_ok {
        return false;
    }
    match fix_filter {
        FixFilter::Any => true,
        FixFilter::Denuvo => fix_flags_by_app.contains(&entry.app_id),
    }
}

pub fn search_result_row(
    ui: &mut egui::Ui,
    entry: &CatalogApp,
    row_height: f32,
    selected: bool,
    headers: &HeaderResolver,
) -> bool {
    let (slot, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_height), Sense::click());
    let rect = slot;
    let hover = ui.ctx().animate_bool(response.id, response.hovered());

    let fill = if selected {
        lerp_color(SURFACE_RAISED, ACCENT, 0.22)
    } else {
        lerp_color(SURFACE, SURFACE_RAISED, 0.35 + hover * 0.65)
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        fill,
        Stroke::new(
            1.0,
            lerp_color(BORDER, ACCENT, if selected { 0.6 } else { hover }),
        ),
        egui::StrokeKind::Inside,
    );

    // Left cyan indicator bar when hovered or selected
    if selected || hover > 0.1 {
        let bar_h = rect.height() - 16.0;
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 4.0, rect.center().y - bar_h / 2.0),
            Vec2::new(3.5, bar_h),
        );
        ui.painter().rect_filled(
            bar_rect,
            egui::CornerRadius::same(2),
            lerp_color(BORDER, ACCENT, hover),
        );
    }

    let pad = 8.0;
    let img_h = (rect.height() - pad * 2.0).max(1.0);
    let img_w = img_h / STEAM_HEADER_ASPECT;
    let img_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad + 6.0, rect.top() + pad),
        Vec2::new(img_w, img_h),
    );
    if ui.clip_rect().expand(row_height * 8.0).intersects(slot) {
        let guesses = steam_artwork_urls(entry.app_id);
        let resolved = headers.get(entry.app_id);
        let mut refs: Vec<&str> = guesses.iter().map(String::as_str).collect();
        if let Some(url) = resolved.as_deref() {
            refs.push(url);
        }
        let all_failed = paint_remote_image_cover_multi(ui, img_rect, &refs, egui::CornerRadius::same(6));
        if all_failed {
            headers.request(entry.app_id);
        }
    } else {
        ui.painter()
            .rect_filled(img_rect, egui::CornerRadius::same(6), SURFACE);
    }

    let text_x = img_rect.right() + 14.0;
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        egui::pos2(text_x, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        &entry.name,
        FontId::proportional(14.5),
        TEXT,
    );

    // App ID pill badge
    let id_rect = egui::Rect::from_min_size(egui::pos2(text_x, rect.center().y + 3.0), Vec2::new(56.0, 16.0));
    painter.rect_filled(
        id_rect,
        egui::CornerRadius::same(4),
        Color32::from_rgba_unmultiplied(10, 24, 38, 220),
    );
    painter.text(
        id_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("APP {}", entry.app_id),
        FontId::monospace(14.0),
        MUTED,
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    response.clicked()
}

/// A card-styled filter dropdown.
pub fn filter_dropdown(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    selected: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(14.0).strong().color(MUTED));
        ui.add_space(5.0);
        let mut style = (**ui.style()).clone();
        let round = egui::CornerRadius::same(10);
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = round;
            widget.bg_fill = SURFACE_RAISED;
            widget.weak_bg_fill = SURFACE_RAISED;
            widget.bg_stroke = Stroke::new(1.0, BORDER);
            widget.expansion = 0.0;
        }
        let lit = Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.55));
        style.visuals.widgets.hovered.bg_stroke = lit;
        style.visuals.widgets.hovered.weak_bg_fill = lerp_color(SURFACE_RAISED, ACCENT, 0.10);
        style.visuals.widgets.active.bg_stroke = lit;
        style.visuals.widgets.open.bg_stroke = lit;
        style.spacing.button_padding = Vec2::new(12.0, 8.0);
        style.spacing.combo_width = width;
        ui.set_style(style);
        egui::ComboBox::from_id_salt(id)
            .selected_text(RichText::new(selected).size(14.0).color(TEXT))
            .width(width)
            .show_ui(ui, add_contents);
    });
}

/// A Steam-style store sub-tab pill widget.
#[allow(dead_code)]
pub fn store_subtab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), TEXT);
    let size = Vec2::new(galley.size().x + 28.0, 38.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());

    let fill = if active {
        lerp_color(SURFACE_RAISED, ACCENT, 0.12)
    } else {
        lerp_color(Color32::TRANSPARENT, SURFACE, hover)
    };
    let stroke = if active {
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.6))
    } else {
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover * 0.4))
    };

    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );

    let color = if active {
        ACCENT_SOFT
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, label, font, color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Modern section header with an aligned accent bar, bold title, aligned subtitle, and glass pill "See All ›" button.
/// Returns true if "See All" was clicked.
pub fn section_header(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>, see_all: bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                // Accent indicator bar aligned specifically with the title text
                let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(3.5, 18.0), Sense::hover());
                ui.painter()
                    .rect_filled(bar_rect, egui::CornerRadius::same(2), ACCENT);
                ui.add_space(8.0);
                ui.label(RichText::new(title).size(20.0).strong().color(Color32::WHITE));
            });
            if let Some(sub) = subtitle {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    // Indent subtitle to align directly under the title text
                    ui.add_space(11.5);
                    ui.label(RichText::new(sub).size(14.0).color(MUTED));
                });
            }
        });

        if see_all {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let text = format!("See All  {}", icons::CHEVRON_RIGHT);
                let font = FontId::proportional(14.0);
                let galley = ui.painter().layout_no_wrap(text.clone(), font.clone(), TEXT);
                let btn_size = Vec2::new(galley.size().x + 24.0, 32.0);
                let (btn_rect, btn_resp) = ui.allocate_exact_size(btn_size, Sense::click());
                let hover = ui.ctx().animate_bool(btn_resp.id, btn_resp.hovered());

                let bg_fill = Color32::from_rgba_unmultiplied(16, 32, 48, (160.0 + 50.0 * hover) as u8);
                let stroke_color = lerp_color(BORDER, ACCENT, 0.35 + hover * 0.65);
                let text_color = lerp_color(MUTED, ACCENT_SOFT, hover);

                ui.painter().rect(
                    btn_rect,
                    egui::CornerRadius::same(16),
                    bg_fill,
                    Stroke::new(1.0, stroke_color),
                    egui::StrokeKind::Inside,
                );

                ui.painter().text(
                    btn_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    font,
                    text_color,
                );

                if btn_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if btn_resp.clicked() {
                    clicked = true;
                }
            });
        }
    });
    clicked
}

/// Modern poster card v2: rounded corners, hover lift + glow border, title & rating overlay.
/// `active` adds a cyan accent ring (used for the current hero in the strip).
pub fn store_poster_card_v2(
    ui: &mut egui::Ui,
    capsule: &StoreCapsule,
    width: f32,
    rating: &str,
    active: bool,
) -> bool {
    let height = width * 1.40;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());

    // Card background + optional hover lift shadow (drawn slightly expanded)
    if hover > 0.05 {
        let shadow = rect.expand(3.0 * hover);
        ui.painter().rect_filled(
            shadow,
            egui::CornerRadius::same(14),
            Color32::from_rgba_unmultiplied(0, 225, 250, (18.0 * hover) as u8),
        );
    }

    let corner = egui::CornerRadius::same(12);
    let border_col = if active {
        lerp_color(BORDER, ACCENT, 0.85)
    } else {
        lerp_color(BORDER, ACCENT, hover * 0.75)
    };
    ui.painter().rect(
        rect,
        corner,
        lerp_color(SURFACE, SURFACE_RAISED, hover),
        Stroke::new(if active { 2.0 } else { 1.0 }, border_col),
        egui::StrokeKind::Inside,
    );

    // Cover art
    let urls = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_600x900.jpg",
            capsule.app_id
        ),
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            capsule.app_id
        ),
        capsule.header_image_url.clone(),
    ];
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    paint_remote_image_cover_multi(ui, rect, &refs, corner);

    let painter = ui.painter().with_clip_rect(rect);

    // Bottom gradient scrim
    let scrim_h = height * 0.42;
    let scrim_strips = 32usize;
    for i in 0..scrim_strips {
        let t = i as f32 / (scrim_strips - 1) as f32;
        let alpha = (200.0 * (1.0 - t).powf(1.2)) as u8;
        if alpha == 0 {
            continue;
        }
        let y0 = rect.bottom() - scrim_h * ((i + 1) as f32 / scrim_strips as f32);
        let y1 = rect.bottom() - scrim_h * (i as f32 / scrim_strips as f32);
        let rnd = if i == scrim_strips - 1 {
            egui::CornerRadius {
                sw: 12,
                se: 12,
                nw: 0,
                ne: 0,
            }
        } else {
            egui::CornerRadius::ZERO
        };
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(rect.left(), y0), egui::pos2(rect.right(), y1)),
            rnd,
            Color32::from_rgba_unmultiplied(4, 10, 18, alpha),
        );
    }

    // Rating badge — top right
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 56.0, rect.top() + 8.0),
        Vec2::new(48.0, 24.0),
    );
    painter.rect_filled(
        badge_rect,
        egui::CornerRadius::same(12),
        Color32::from_rgba_unmultiplied(4, 10, 18, 210),
    );
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{} {rating}", icons::STAR),
        FontId::proportional(14.0),
        AMBER,
    );

    // Game title — bottom left, two lines max
    let title_short = ellipsize(&capsule.name, 20);
    painter.text(
        egui::pos2(rect.left() + 9.0, rect.bottom() - 12.0),
        egui::Align2::LEFT_BOTTOM,
        title_short,
        FontId::proportional(14.0),
        if hover > 0.4 { ACCENT_SOFT } else { TEXT },
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// Old poster card — kept for compatibility with any remaining callers.
#[allow(dead_code)]
pub fn store_poster_card(ui: &mut egui::Ui, capsule: &StoreCapsule, width: f32, rating: &str) -> bool {
    store_poster_card_v2(ui, capsule, width, rating, false)
}

#[allow(dead_code)]
pub fn store_banner(
    ui: &mut egui::Ui,
    capsule: &StoreCapsule,
    rank: usize,
    activatable: bool,
) -> Option<StoreAction> {
    let width = ui.available_width();
    let height = width / STORE_HERO_ASPECT;
    store_banner_sized(ui, capsule, rank, activatable, Vec2::new(width, height))
}

pub fn store_banner_sized(
    ui: &mut egui::Ui,
    capsule: &StoreCapsule,
    rank: usize,
    activatable: bool,
    size: Vec2,
) -> Option<StoreAction> {
    let mut action = None;
    let width = size.x;
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let corner = egui::CornerRadius::same(14);

    let urls = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
            capsule.app_id
        ),
        capsule.header_image_url.clone(),
    ];
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    paint_remote_image_cover_multi(ui, rect, &refs, corner);

    let response = ui.interact(rect, ui.id().with(("banner", capsule.app_id)), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let painter = ui.painter().with_clip_rect(rect);

    // Darkening gradient overlays (Left gradient + Bottom gradient)
    let fade_w = width * 0.65;
    let strips = 48;
    for i in 0..strips {
        let t = i as f32 / (strips - 1) as f32;
        let alpha = (190.0 * (1.0 - t).powf(1.3)) as u8;
        if alpha == 0 {
            continue;
        }
        let x0 = rect.left() + fade_w * (i as f32 / strips as f32);
        let x1 = rect.left() + fade_w * ((i + 1) as f32 / strips as f32);
        let round = if i == 0 {
            egui::CornerRadius {
                nw: 14,
                sw: 14,
                ne: 0,
                se: 0,
            }
        } else {
            egui::CornerRadius::ZERO
        };
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            round,
            Color32::from_rgba_unmultiplied(6, 12, 20, alpha),
        );
    }

    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
        egui::StrokeKind::Inside,
    );

    let left = rect.left() + 32.0;

    // Eyebrow chips (Rank + Status)
    let eyebrow_text = format!("{} #{rank} TOP SELLER", icons::FLAME);
    painter.text(
        egui::pos2(left, rect.top() + 36.0),
        egui::Align2::LEFT_TOP,
        eyebrow_text,
        FontId::monospace(14.0),
        AMBER,
    );

    if activatable {
        let tag_rect = egui::Rect::from_min_size(
            egui::pos2(left + 160.0, rect.top() + 32.0),
            Vec2::new(230.0, 26.0),
        );
        painter.rect_filled(
            tag_rect,
            egui::CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(0, 225, 250, 35),
        );
        painter.text(
            tag_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{} ACTIVATABLE IN DRYDOCK", icons::SHIELD),
            FontId::monospace(14.0),
            ACCENT_SOFT,
        );
    }

    // Title
    painter.text(
        egui::pos2(left, rect.top() + 64.0),
        egui::Align2::LEFT_TOP,
        &capsule.name,
        FontId::proportional(32.0),
        Color32::WHITE,
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        action = Some(StoreAction::Details(capsule.app_id));
    }

    // Action buttons bottom-left
    let (label, primary) = if activatable {
        ("▶ ACTIVATE IN DRYDOCK", true)
    } else {
        ("VIEW IN STORE", false)
    };
    let btn_w = if activatable { 190.0 } else { 140.0 };
    let btn_rect = egui::Rect::from_min_size(egui::pos2(left, rect.bottom() - 52.0), Vec2::new(btn_w, 36.0));
    let button = if primary {
        success_button(label).min_size(Vec2::new(btn_w, 36.0))
    } else {
        ghost_button(label).min_size(Vec2::new(btn_w, 36.0))
    };
    if ui.put(btn_rect, button).clicked() {
        action = Some(if activatable {
            StoreAction::Activate(capsule.app_id)
        } else {
            StoreAction::Details(capsule.app_id)
        });
    }

    // Secondary action button (DETAILS + icon)
    let sec_rect = egui::Rect::from_min_size(
        egui::pos2(left + btn_w + 12.0, rect.bottom() - 52.0),
        Vec2::new(120.0, 36.0),
    );
    if ui
        .put(
            sec_rect,
            ghost_button(&format!("DETAILS  {}", icons::CHEVRON_RIGHT)).min_size(Vec2::new(120.0, 36.0)),
        )
        .clicked()
    {
        action = Some(StoreAction::Details(capsule.app_id));
    }

    action
}

/// Height of a Steam-style list row.
pub const LIST_ROW_HEIGHT: f32 = 62.0;
/// Transparent gap baked below each row.
pub const LIST_ROW_GAP: f32 = 5.0;

#[allow(dead_code)]
pub fn list_row_base(
    ui: &mut egui::Ui,
    app_id: u32,
    preferred_thumb: Option<&str>,
    title: &str,
    meta: &str,
    meta_color: Color32,
    right_w: f32,
) -> (egui::Rect, egui::Response) {
    let (rect, hover_resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), LIST_ROW_HEIGHT), Sense::hover());
    let hover = ui.ctx().animate_bool(hover_resp.id, hover_resp.hovered());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        lerp_color(SURFACE, SURFACE_RAISED, 0.35 + hover * 0.65),
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
        egui::StrokeKind::Inside,
    );
    let th = LIST_ROW_HEIGHT - 16.0;
    let tw = th / STEAM_HEADER_ASPECT;
    let thumb = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 9.0, rect.center().y - th / 2.0),
        Vec2::new(tw, th),
    );
    let fallback = steam_artwork_urls(app_id);
    let mut refs: Vec<&str> = Vec::with_capacity(5);
    if let Some(url) = preferred_thumb.filter(|url| url.starts_with("https://")) {
        refs.push(url);
    }
    refs.extend(fallback.iter().map(String::as_str));
    paint_remote_image_cover_multi(ui, thumb, &refs, egui::CornerRadius::same(4));

    let text_x = thumb.right() + 16.0;
    let painter = ui.painter().with_clip_rect(rect);
    let has_meta = !meta.is_empty();
    let title_y = if has_meta {
        rect.center().y - 9.0
    } else {
        rect.center().y
    };
    painter.text(
        egui::pos2(text_x, title_y),
        egui::Align2::LEFT_CENTER,
        title,
        FontId::proportional(15.0),
        TEXT,
    );
    if has_meta {
        painter.text(
            egui::pos2(text_x, rect.center().y + 11.0),
            egui::Align2::LEFT_CENTER,
            meta,
            FontId::proportional(14.0),
            meta_color,
        );
    }

    let right_zone = egui::Rect::from_min_size(
        egui::pos2(rect.right() - right_w - 12.0, rect.center().y - 16.0),
        Vec2::new(right_w, 32.0),
    );
    let click_right = if right_w > 0.0 {
        right_zone.left() - 8.0
    } else {
        rect.right()
    };
    let click_rect = egui::Rect::from_min_max(rect.min, egui::pos2(click_right, rect.bottom()));
    let click = ui.interact(click_rect, hover_resp.id.with("click"), Sense::click());
    if click.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (right_zone, click)
}

#[allow(dead_code)]
pub fn store_list_row(ui: &mut egui::Ui, capsule: &StoreCapsule, activatable: bool) -> Option<StoreAction> {
    let (meta, meta_color) = if activatable {
        ("Activatable in Drydock", ACCENT_SOFT)
    } else {
        ("", MUTED)
    };
    let (label, primary, btn_w) = if activatable {
        ("ACTIVATE", true, 116.0)
    } else {
        ("VIEW", false, 84.0)
    };
    let mut action = None;
    let width = ui.available_width();
    list_row_slot(ui, width, |ui| {
        let (right_zone, click) = list_row_base(
            ui,
            capsule.app_id,
            Some(capsule.header_image_url.as_str()),
            &capsule.name,
            meta,
            meta_color,
            btn_w,
        );
        if click.clicked() {
            action = Some(StoreAction::Details(capsule.app_id));
        }
        let button = if primary {
            success_button(label).min_size(Vec2::new(btn_w, 32.0))
        } else {
            ghost_button(label).min_size(Vec2::new(btn_w, 32.0))
        };
        if ui.put(right_zone, button).clicked() {
            action = Some(if activatable {
                StoreAction::Activate(capsule.app_id)
            } else {
                StoreAction::Details(capsule.app_id)
            });
        }
    });
    ui.add_space(LIST_ROW_GAP);
    action
}

/// Runs `contents` inside a fixed-height row slot (`LIST_ROW_HEIGHT`).
pub fn list_row_slot(ui: &mut egui::Ui, width: f32, contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, LIST_ROW_HEIGHT),
        Layout::top_down(Align::Min),
        |ui| {
            ui.set_width(width);
            contents(ui);
        },
    );
}

impl DrydockApp {
    /// A modern, prominent search bar positioned at the top of the Home / Storefront page.
    pub fn home_search_bar(&mut self, ui: &mut egui::Ui) {
        let is_searching = !self.search.trim().is_empty();
        let edit_id = ui.id().with("home_search_input");
        let is_focused = ui.ctx().memory(|m| m.has_focus(edit_id));

        // Global shortcuts:
        // Ctrl+K (or Cmd+K) focuses search bar
        // "/" focuses search bar if not already typing in another input
        if ui.input(|i| {
            ((i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::K))
                || (!is_focused && !is_searching && i.key_pressed(egui::Key::Slash))
        }) {
            ui.ctx().memory_mut(|m| m.request_focus(edit_id));
        }

        // Escape clears search and unfocuses
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && (is_searching || is_focused) {
            if is_searching {
                self.search.clear();
            }
            ui.ctx().memory_mut(|m| m.surrender_focus(edit_id));
        }

        let bar_id = ui.id().with("home_search_bar_container");
        let bar_height = 48.0;
        let bar_w = ui.available_width();
        let (bar_rect, bar_resp) = ui.allocate_exact_size(Vec2::new(bar_w, bar_height), Sense::click());

        let hover = ui
            .ctx()
            .animate_bool(bar_id, bar_resp.hovered() || is_focused || is_searching);

        let border_color = if is_focused {
            ACCENT
        } else if is_searching {
            lerp_color(BORDER, ACCENT, 0.6)
        } else {
            lerp_color(BORDER, ACCENT, hover * 0.45)
        };
        let stroke_width = if is_focused { 1.5 } else { 1.0 };

        let fill_color = if is_focused {
            lerp_color(SURFACE_RAISED, ACCENT, 0.07)
        } else if is_searching {
            lerp_color(SURFACE_RAISED, ACCENT, 0.04)
        } else {
            lerp_color(SURFACE, SURFACE_RAISED, 0.5 + hover * 0.5)
        };

        // Paint container background and rounded border
        ui.painter().rect(
            bar_rect,
            14.0,
            fill_color,
            Stroke::new(stroke_width, border_color),
            egui::StrokeKind::Inside,
        );

        // Click anywhere on padding/blank area of the search bar to focus the TextEdit
        if bar_resp.clicked() && !is_focused {
            ui.ctx().memory_mut(|m| m.request_focus(edit_id));
        }

        // Layout inner elements centered vertically
        let inner_rect = bar_rect.shrink2(Vec2::new(16.0, 7.0));
        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner_rect));

        child_ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;

            // Search Icon (glows when focused)
            let icon_color = if is_focused {
                ACCENT
            } else if is_searching {
                ACCENT_SOFT
            } else {
                lerp_color(MUTED, TEXT, hover * 0.45)
            };
            ui.label(RichText::new(icons::SEARCH).size(18.0).color(icon_color));

            // Reserve room for right-side buttons when searching
            let right_reserved = if is_searching { 130.0 } else { 0.0 };
            let edit_w = (ui.available_width() - right_reserved).max(100.0);

            let edit = egui::TextEdit::singleline(&mut self.search)
                .id(edit_id)
                .hint_text("Search games by title, genre, or Steam App ID…")
                .font(FontId::proportional(15.0))
                .text_color(TEXT)
                .desired_width(edit_w)
                .margin(egui::Margin::ZERO)
                .frame(egui::Frame::NONE);
            ui.add(edit);

            if is_searching {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    // ESC keycap badge
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 12))
                        .stroke(Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
                        ))
                        .corner_radius(4)
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .show(ui, |ui| {
                            ui.label(RichText::new("ESC").size(11.0).strong().color(MUTED));
                        });

                    // Clear button
                    let clear_resp = ui.add(
                        egui::Button::new(
                            RichText::new(format!("{}  Clear", icons::CLOSE))
                                .size(13.5)
                                .color(ACCENT),
                        )
                        .frame(false),
                    );
                    if clear_resp.clicked() {
                        self.search.clear();
                    }
                    if clear_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    clear_resp.on_hover_text("Clear search query");
                });
            }
        });
    }

    pub fn home_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        self.home_search_bar(ui);
        ui.add_space(18.0);

        if !self.search.trim().is_empty() {
            self.store_search_results(ui);
            return;
        }
        self.start_featured();
        self.home_store_column(ui);
    }

    /// A small "loading / offline" line shared by the storefront tabs.
    pub fn store_feed_status(&self, ui: &mut egui::Ui) -> bool {
        if self.featured.is_some() {
            return true;
        }
        if self.featured_loading {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Loading the live storefront from Steam…")
                        .size(14.0)
                        .color(MUTED),
                );
            });
        } else if let Some(error) = &self.featured_error {
            ui.label(
                RichText::new(format!("Steam storefront is unavailable: {error}"))
                    .size(14.0)
                    .color(AMBER),
            );
        }
        false
    }

    /// The unified home storefront page.
    pub fn home_store_column(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);

        let featured = self.featured.clone();
        if !self.store_feed_status(ui) {
            return;
        }

        let feed = match featured.as_ref() {
            Some(f) => f,
            None => return,
        };

        if self.catalog.is_empty() {
            ui.label(RichText::new("Loading the game list…").color(MUTED));
            return;
        }

        let available: Vec<&StoreCapsule> = feed
            .top_sellers
            .iter()
            .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
            .collect();

        if available.is_empty() {
            ui.label(
                RichText::new("None of Steam's top sellers are available in Drydock right now.").color(MUTED),
            );
            return;
        }

        // ── Auto-scroll timer: cycle the hero every 5 s ─────────────────────────────────────
        let now = std::time::Instant::now();
        let should_advance = match self.hero_last_scroll {
            Some(last) => now.duration_since(last) >= std::time::Duration::from_secs(5),
            None => true,
        };
        if should_advance {
            self.hero_carousel_index = (self.hero_carousel_index + 1) % available.len();
            self.hero_last_scroll = Some(now);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));

        let current_index = self.hero_carousel_index % available.len();
        let hero = available[current_index];
        let mut action = None;

        // ════════════════════════════════════════════════════════════════════════════════
        // SECTION 1 – FEATURED GAMES  (full-width hero banner + scrollable poster strip)
        // ════════════════════════════════════════════════════════════════════════════════
        if section_header(ui, "Featured Games", None, true) {
            self.open_see_all(SeeAllSection::Featured);
        }
        ui.add_space(14.0);

        // ── Hero banner ──────────────────────────────────────────────────────────────────
        {
            let total_w = ui.available_width();
            let hero_h = (total_w / 2.5).clamp(240.0, 360.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(total_w, hero_h), Sense::hover());
            let corner = egui::CornerRadius::same(16);

            let urls = [
                format!(
                    "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
                    hero.app_id
                ),
                hero.header_image_url.clone(),
            ];
            let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
            paint_remote_image_cover_multi(ui, rect, &refs, corner);

            let response = ui.interact(rect, ui.id().with(("hero_banner", hero.app_id)), Sense::click());
            let hover = ui.ctx().animate_bool(response.id, response.hovered());
            let painter = ui.painter().with_clip_rect(rect);

            // Left-to-right deep dark vignette (for maximum text contrast on the left side)
            let vignette_w = total_w * 0.65;
            for i in 0..36usize {
                let t = i as f32 / 35.0;
                let alpha = (235.0 * (1.0 - t).powf(1.4)) as u8;
                if alpha == 0 {
                    continue;
                }
                let x0 = rect.left() + vignette_w * (i as f32 / 36.0);
                let x1 = rect.left() + vignette_w * ((i + 1) as f32 / 36.0);
                let rnd = if i == 0 {
                    egui::CornerRadius {
                        nw: 16,
                        sw: 16,
                        ne: 0,
                        se: 0,
                    }
                } else {
                    egui::CornerRadius::ZERO
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
                    rnd,
                    Color32::from_rgba_unmultiplied(6, 12, 20, alpha),
                );
            }

            // Bottom-to-top dark scrim for controls and pagination
            let grad_h = hero_h * 0.65;
            for i in 0..32usize {
                let t = i as f32 / 31.0;
                let alpha = (210.0 * (1.0 - t).powf(1.2)) as u8;
                if alpha == 0 {
                    continue;
                }
                let y0 = rect.bottom() - grad_h * ((i + 1) as f32 / 32.0);
                let y1 = rect.bottom() - grad_h * (i as f32 / 32.0);
                let rnd = if i == 31 {
                    egui::CornerRadius {
                        sw: 16,
                        se: 16,
                        nw: 0,
                        ne: 0,
                    }
                } else {
                    egui::CornerRadius::ZERO
                };
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(rect.left(), y0), egui::pos2(rect.right(), y1)),
                    rnd,
                    Color32::from_rgba_unmultiplied(6, 12, 20, alpha),
                );
            }

            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(1.5, lerp_color(BORDER, ACCENT, hover * 0.8)),
                egui::StrokeKind::Inside,
            );

            // Left content starts at rect.left() + 56.0 — COMPLETELY CLEARS THE LEFT ARROW BUTTON!
            let left = rect.left() + 56.0;

            // Eyebrow chips: Rank
            let chip_y = rect.top() + 34.0;
            let chip_x = left;

            // 1. Top seller chip
            let chip_w = 175.0;
            let chip_rect = egui::Rect::from_min_size(egui::pos2(chip_x, chip_y), Vec2::new(chip_w, 28.0));
            painter.rect_filled(
                chip_rect,
                egui::CornerRadius::same(6),
                Color32::from_rgba_unmultiplied(245, 158, 11, 35),
            );
            painter.rect_stroke(
                chip_rect,
                egui::CornerRadius::same(6),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(245, 158, 11, 100)),
                egui::StrokeKind::Inside,
            );
            painter.text(
                chip_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{}  #{} TOP SELLER", icons::FLAME, current_index + 1),
                FontId::monospace(14.0),
                AMBER,
            );

            // Title
            let title_y = rect.top() + 74.0;
            painter.text(
                egui::pos2(left, title_y),
                egui::Align2::LEFT_TOP,
                &hero.name,
                FontId::proportional(32.0),
                Color32::WHITE,
            );

            // Price / Discount Row
            let price_y = title_y + 44.0;
            let mut price_x = left;
            if hero.discount_percent > 0 {
                let disc_rect =
                    egui::Rect::from_min_size(egui::pos2(price_x, price_y), Vec2::new(54.0, 24.0));
                painter.rect_filled(disc_rect, egui::CornerRadius::same(4), VERDIGRIS);
                painter.text(
                    disc_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("-{}%", hero.discount_percent),
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
                price_x += 62.0;
            }
            if !hero.price.is_empty() {
                painter.text(
                    egui::pos2(price_x, price_y + 12.0),
                    egui::Align2::LEFT_CENTER,
                    &hero.price,
                    FontId::proportional(16.0),
                    TEXT,
                );
            }

            // Action buttons bottom-left
            let btn_y = rect.bottom() - 56.0;
            let explore_rect = egui::Rect::from_min_size(egui::pos2(left, btn_y), Vec2::new(170.0, 38.0));
            if ui
                .put(
                    explore_rect,
                    primary_button(&format!("{}  EXPLORE GAME", icons::STORE))
                        .min_size(Vec2::new(170.0, 38.0)),
                )
                .clicked()
                || response.clicked()
            {
                action = Some(StoreAction::Details(hero.app_id));
            }

            // Arrow buttons: Far edges, glassmorphic 38px
            let arrow_y = rect.center().y;
            let arrow_sz = Vec2::new(38.0, 38.0);
            let la_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, arrow_y), arrow_sz);
            let ra_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 18.0, arrow_y), arrow_sz);

            let la = ui.interact(la_rect, ui.id().with("hero_prev"), Sense::click());
            let ra = ui.interact(ra_rect, ui.id().with("hero_next"), Sense::click());
            let lh = ui.ctx().animate_bool(la.id, la.hovered());
            let rh = ui.ctx().animate_bool(ra.id, ra.hovered());

            let la_fill = lerp_color(
                Color32::from_rgba_unmultiplied(6, 14, 24, 180),
                Color32::from_rgba_unmultiplied(0, 225, 250, 45),
                lh,
            );
            let ra_fill = lerp_color(
                Color32::from_rgba_unmultiplied(6, 14, 24, 180),
                Color32::from_rgba_unmultiplied(0, 225, 250, 45),
                rh,
            );
            let la_stroke = Stroke::new(1.0, lerp_color(BORDER, ACCENT, lh));
            let ra_stroke = Stroke::new(1.0, lerp_color(BORDER, ACCENT, rh));

            painter.rect(
                la_rect,
                egui::CornerRadius::same(19),
                la_fill,
                la_stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect(
                ra_rect,
                egui::CornerRadius::same(19),
                ra_fill,
                ra_stroke,
                egui::StrokeKind::Inside,
            );

            painter.text(
                la_rect.center(),
                egui::Align2::CENTER_CENTER,
                icons::CHEVRON_LEFT,
                FontId::proportional(20.0),
                lerp_color(MUTED, TEXT, lh),
            );
            painter.text(
                ra_rect.center(),
                egui::Align2::CENTER_CENTER,
                icons::CHEVRON_RIGHT,
                FontId::proportional(20.0),
                lerp_color(MUTED, TEXT, rh),
            );

            if la.hovered() || ra.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if la.clicked() {
                self.hero_carousel_index = if self.hero_carousel_index == 0 {
                    available.len() - 1
                } else {
                    self.hero_carousel_index - 1
                };
                self.hero_last_scroll = Some(std::time::Instant::now());
            }
            if ra.clicked() {
                self.hero_carousel_index = (self.hero_carousel_index + 1) % available.len();
                self.hero_last_scroll = Some(std::time::Instant::now());
            }

            // Modern Pill Pagination Dock (dynamic sliding window that always follows current_index)
            let total_items = available.len();
            let max_visible = 9.min(total_items);
            let pill_h = 6.0;

            let start_idx = if total_items <= max_visible {
                0
            } else {
                let half = max_visible / 2;
                if current_index <= half {
                    0
                } else if current_index >= total_items - half {
                    total_items - max_visible
                } else {
                    current_index - half
                }
            };
            let end_idx = (start_idx + max_visible).min(total_items);

            let mut total_dock_w = 0.0;
            for i in start_idx..end_idx {
                let is_cur = i == current_index;
                let is_edge = total_items > max_visible
                    && ((i == start_idx && start_idx > 0) || (i == end_idx - 1 && end_idx < total_items));
                let pill_w = if is_cur {
                    24.0
                } else if is_edge {
                    4.0
                } else {
                    7.0
                };
                total_dock_w += pill_w;
                if i + 1 < end_idx {
                    total_dock_w += 7.0;
                }
            }

            let dock_x0 = rect.center().x - total_dock_w / 2.0;
            let dock_y = rect.bottom() - 18.0;

            let mut cur_x = dock_x0;
            for i in start_idx..end_idx {
                let is_cur = i == current_index;
                let is_edge = total_items > max_visible
                    && ((i == start_idx && start_idx > 0) || (i == end_idx - 1 && end_idx < total_items));
                let pill_w = if is_cur {
                    24.0
                } else if is_edge {
                    4.0
                } else {
                    7.0
                };
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        cur_x,
                        dock_y + (pill_h - if is_edge { 4.0 } else { pill_h }) / 2.0,
                    ),
                    Vec2::new(pill_w, if is_edge { 4.0 } else { pill_h }),
                );
                let p_resp = ui.interact(
                    pill_rect.expand(4.0),
                    ui.id().with(("hero_dot", i)),
                    Sense::click(),
                );
                let p_hov = ui.ctx().animate_bool(p_resp.id, p_resp.hovered());

                let col = if is_cur {
                    ACCENT
                } else if is_edge {
                    Color32::from_rgba_unmultiplied(180, 200, 220, 50)
                } else {
                    lerp_color(
                        Color32::from_rgba_unmultiplied(200, 220, 240, 70),
                        ACCENT_SOFT,
                        p_hov,
                    )
                };
                painter.rect_filled(pill_rect, egui::CornerRadius::same(3), col);
                if p_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if p_resp.clicked() {
                    self.hero_carousel_index = i;
                    self.hero_last_scroll = Some(std::time::Instant::now());
                }
                cur_x += pill_w + 7.0;
            }

            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        ui.add_space(16.0);

        // ── Thumbnail poster strip below hero ────────────────────────────────────────────
        {
            let ratings = [
                "9.8", "9.5", "9.2", "9.6", "9.0", "9.4", "8.9", "9.3", "8.7", "9.1",
            ];
            egui::ScrollArea::horizontal()
                .id_salt("featured_strip")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .scroll_source(egui::scroll_area::ScrollSource {
                    scroll_bar: false,
                    drag: egui::scroll_area::DragScroll::Always,
                    mouse_wheel: true,
                })
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        for (idx, capsule) in available.iter().enumerate() {
                            let rating = ratings[idx % ratings.len()];
                            let is_active = idx == current_index;
                            if store_poster_card_v2(ui, capsule, 160.0, rating, is_active) {
                                self.hero_carousel_index = idx;
                                self.hero_last_scroll = Some(std::time::Instant::now());
                                action = Some(StoreAction::Details(capsule.app_id));
                            }
                        }
                    });
                    ui.add_space(4.0);
                });
        }

        ui.add_space(36.0);

        // ════════════════════════════════════════════════════════════════════════════════
        // SECTION 2 – TOP PICKS FOR YOU
        // ════════════════════════════════════════════════════════════════════════════════
        if section_header(
            ui,
            "Top Picks For You",
            Some("Discover games curated only for you"),
            true,
        ) {
            self.open_see_all(SeeAllSection::TopPicks);
        }
        ui.add_space(14.0);

        egui::ScrollArea::horizontal()
            .id_salt("top_picks_strip")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .scroll_source(egui::scroll_area::ScrollSource {
                scroll_bar: false,
                drag: egui::scroll_area::DragScroll::Always,
                mouse_wheel: true,
            })
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let pick_ratings = [
                        "9.7", "9.1", "8.0", "7.7", "9.5", "9.3", "8.9", "9.2", "8.6", "9.4",
                    ];
                    for (idx, capsule) in available.iter().enumerate() {
                        let rating = pick_ratings[idx % pick_ratings.len()];
                        if store_poster_card_v2(ui, capsule, 172.0, rating, false) {
                            action = Some(StoreAction::Details(capsule.app_id));
                        }
                    }
                });
                ui.add_space(4.0);
            });

        ui.add_space(36.0);

        // ════════════════════════════════════════════════════════════════════════════════
        // SECTION 3 – NEW RELEASES
        // ════════════════════════════════════════════════════════════════════════════════
        let new_releases: Vec<&StoreCapsule> = feed
            .new_releases
            .iter()
            .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
            .collect();

        if !new_releases.is_empty() {
            if section_header(
                ui,
                "New Releases",
                Some("Fresh from the Steam top sellers feed"),
                true,
            ) {
                self.open_see_all(SeeAllSection::NewReleases);
            }
            ui.add_space(14.0);

            egui::ScrollArea::horizontal()
                .id_salt("new_releases_strip")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .scroll_source(egui::scroll_area::ScrollSource {
                    scroll_bar: false,
                    drag: egui::scroll_area::DragScroll::Always,
                    mouse_wheel: true,
                })
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        let nr_ratings = [
                            "8.8", "9.3", "9.0", "8.5", "9.2", "9.6", "8.7", "9.1", "8.4", "9.4",
                        ];
                        for (idx, capsule) in new_releases.iter().enumerate() {
                            let rating = nr_ratings[idx % nr_ratings.len()];
                            if store_poster_card_v2(ui, capsule, 172.0, rating, false) {
                                action = Some(StoreAction::Details(capsule.app_id));
                            }
                        }
                    });
                    ui.add_space(4.0);
                });

            ui.add_space(36.0);
        }

        // ════════════════════════════════════════════════════════════════════════════════
        // SECTION 4 – REPACKS & DENUVO WATCH
        // ════════════════════════════════════════════════════════════════════════════════
        {
            let rc = self.repackers_by_app.len();
            let dc = self.denuvo_appids.len();
            let sub = format!("{rc} repacks · {dc} with Denuvo");
            section_header(ui, "Repacks & Denuvo Watch", Some(sub.as_str()), false);
            ui.add_space(14.0);

            let denuvo_games: Vec<&CatalogApp> = self
                .catalog
                .iter()
                .filter(|entry| self.denuvo_appids.contains(&entry.app_id))
                .take(6)
                .collect();

            ui.spacing_mut().item_spacing.y = 0.0;
            let row_w = ui.available_width();
            for entry in denuvo_games {
                let selected = self.selected_app == Some(entry.app_id);
                let mut clicked = false;
                list_row_slot(ui, row_w, |ui| {
                    clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
                });
                ui.add_space(LIST_ROW_GAP);
                if clicked {
                    action = Some(StoreAction::Details(entry.app_id));
                }
            }
        }

        ui.add_space(32.0);

        match action {
            Some(StoreAction::Details(app_id)) => self.open_details(app_id),
            Some(StoreAction::Activate(app_id)) => self.go_to_activation(app_id),
            None => {}
        }
    }

    /// A store category tab (New Releases): the live category as a price-free Steam-style list.
    #[allow(dead_code)]
    pub fn store_shelf(&self, ui: &mut egui::Ui, capsules: Option<&[StoreCapsule]>) -> Option<StoreAction> {
        if !self.store_feed_status(ui) {
            return None;
        }
        let capsules = capsules.unwrap_or_default();
        if capsules.is_empty() {
            ui.label(RichText::new("Nothing to show in this category right now.").color(MUTED));
            return None;
        }
        let available: Vec<&StoreCapsule> = capsules
            .iter()
            .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
            .collect();
        if available.is_empty() {
            ui.label(
                RichText::new("None of this category's games are available in Drydock right now.")
                    .color(MUTED),
            );
            return None;
        }
        let mut action = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        for capsule in available {
            if let Some(hit) = store_list_row(ui, capsule, false) {
                action = Some(hit);
            }
        }
        ui.add_space(24.0);
        action
    }

    /// The Drydock-only Denuvo Watch tab.
    #[allow(dead_code)]
    pub fn store_denuvo_watch(&self, ui: &mut egui::Ui) -> Option<StoreAction> {
        if !self.denuvo_loaded {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Loading the Denuvo watch list…")
                        .size(14.0)
                        .color(MUTED),
                );
            });
            return None;
        }
        ui.label(
            RichText::new(format!(
                "{} games currently ship with Denuvo — these are the titles Drydock activates.",
                group_thousands(self.denuvo_appids.len())
            ))
            .size(14.0)
            .color(MUTED),
        );
        ui.add_space(14.0);
        let mut games: Vec<&CatalogApp> = self
            .catalog
            .iter()
            .filter(|entry| self.denuvo_appids.contains(&entry.app_id))
            .collect();
        games.sort_by_key(|entry| entry.name.to_lowercase());
        let mut action = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width();
        for entry in games.iter().take(400) {
            let selected = self.selected_app == Some(entry.app_id);
            let mut clicked = false;
            list_row_slot(ui, width, |ui| {
                clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
            });
            ui.add_space(LIST_ROW_GAP);
            if clicked {
                action = Some(StoreAction::Details(entry.app_id));
            }
        }
        action
    }

    /// The Repacks tab.
    #[allow(dead_code)]
    pub fn store_repacks(&self, ui: &mut egui::Ui) -> Option<StoreAction> {
        if !self.repacks_loaded {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the repack list…").size(14.0).color(MUTED));
            });
            return None;
        }
        let mut games: Vec<&CatalogApp> = self
            .catalog
            .iter()
            .filter(|entry| self.repackers_by_app.contains_key(&entry.app_id))
            .collect();
        games.sort_by_key(|entry| entry.name.to_lowercase());
        if games.is_empty() {
            ui.label(RichText::new("No repacks available right now.").color(MUTED));
            return None;
        }
        ui.label(
            RichText::new(format!(
                "{} games have a repack download available.",
                group_thousands(games.len())
            ))
            .size(14.0)
            .color(MUTED),
        );
        ui.add_space(14.0);
        let mut action = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width();
        for entry in games.iter().take(400) {
            let selected = self.selected_app == Some(entry.app_id);
            let mut clicked = false;
            list_row_slot(ui, width, |ui| {
                clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
            });
            ui.add_space(LIST_ROW_GAP);
            if clicked {
                action = Some(StoreAction::Details(entry.app_id));
            }
        }
        action
    }

    /// The nav-search results view.
    pub fn store_search_results(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        self.home_filter_row(ui);
        ui.add_space(12.0);
        let query = self.search.trim().to_lowercase();
        let open = {
            let repack_filter = self.repack_filter.clone();
            let fix_filter = self.fix_filter;
            let repackers = &self.repackers_by_app;
            let fix_flags = &self.fix_flags_by_app;
            let matches: Vec<&CatalogApp> = self
                .catalog
                .iter()
                .filter(|entry| {
                    catalog_matches_filters(entry, &repack_filter, fix_filter, repackers, fix_flags)
                        && (query.is_empty()
                            || entry.name.to_lowercase().contains(&query)
                            || entry.app_id.to_string().contains(&query))
                })
                .take(200)
                .collect();
            ui.label(
                RichText::new(format!(
                    "{} result{}",
                    matches.len(),
                    if matches.len() == 1 { "" } else { "s" }
                ))
                .size(14.0)
                .color(MUTED),
            );
            ui.add_space(8.0);
            let mut open = None;
            if matches.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("No games match your search.").color(MUTED));
                ui.add_space(6.0);
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            for entry in &matches {
                let selected = self.selected_app == Some(entry.app_id);
                let mut clicked = false;
                list_row_slot(ui, width, |ui| {
                    clicked = search_result_row(ui, entry, LIST_ROW_HEIGHT, selected, &self.header_resolver);
                });
                ui.add_space(LIST_ROW_GAP);
                if clicked {
                    open = Some(entry.app_id);
                }
            }
            open
        };
        if let Some(app_id) = open {
            self.open_details(app_id);
        }
    }

    pub fn home_filter_row(&mut self, ui: &mut egui::Ui) {
        let active = self.repack_filter != RepackFilter::Any || self.fix_filter != FixFilter::Any;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;

            filter_dropdown(
                ui,
                "home_repack_filter",
                "REPACKS",
                &self.repack_filter.label(),
                168.0,
                |ui| {
                    ui.selectable_value(&mut self.repack_filter, RepackFilter::Any, "Any");
                    ui.selectable_value(&mut self.repack_filter, RepackFilter::AnyRepack, "All repacks");
                    for name in &self.available_repackers {
                        let selected = self.repack_filter == RepackFilter::Repacker(name.clone());
                        if ui.selectable_label(selected, name).clicked() {
                            self.repack_filter = RepackFilter::Repacker(name.clone());
                        }
                    }
                },
            );

            filter_dropdown(
                ui,
                "home_fix_filter",
                "FIXES",
                self.fix_filter.label(),
                148.0,
                |ui| {
                    ui.selectable_value(&mut self.fix_filter, FixFilter::Any, "Any");
                    ui.selectable_value(&mut self.fix_filter, FixFilter::Denuvo, "Denuvo");
                },
            );

            if active {
                ui.vertical(|ui| {
                    ui.add_space(17.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("{}  Clear", icons::CLOSE))
                                    .size(14.0)
                                    .color(ACCENT),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        self.repack_filter = RepackFilter::Any;
                        self.fix_filter = FixFilter::Any;
                    }
                });
            }
        });
    }

    /// Navigates to the See All page for the given section.
    pub fn open_see_all(&mut self, section: SeeAllSection) {
        self.see_all_section = Some(section);
        self.page = Page::SeeAll;
    }

    /// The "See All" full collection page for a store section.
    pub fn home_see_all_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);

        let featured = self.featured.clone();
        if !self.store_feed_status(ui) {
            return;
        }

        let feed = match featured.as_ref() {
            Some(f) => f,
            None => return,
        };

        let section = self.see_all_section.unwrap_or(SeeAllSection::Featured);

        // Games for the active section
        let games: Vec<&StoreCapsule> = match section {
            SeeAllSection::Featured | SeeAllSection::TopPicks => feed
                .top_sellers
                .iter()
                .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
                .collect(),
            SeeAllSection::NewReleases => feed
                .new_releases
                .iter()
                .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
                .collect(),
        };

        if games.is_empty() {
            ui.horizontal(|ui| {
                if back_button(ui, "Back to Store").clicked() {
                    self.page = Page::Home;
                }
                ui.add_space(10.0);
                ui.label(RichText::new("No games available in this section right now.").color(MUTED));
            });
            return;
        }

        // ── Modern Header: Back button + Title & Count on left, Tabs on right ────────
        ui.horizontal(|ui| {
            if back_button(ui, "Back to Store").clicked() {
                self.page = Page::Home;
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(section.title()).size(22.0).strong().color(TEXT));
                ui.add_space(1.0);
                ui.label(
                    RichText::new(format!("{} games available in Drydock", games.len()))
                        .size(14.0)
                        .color(MUTED),
                );
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for s in [
                    SeeAllSection::NewReleases,
                    SeeAllSection::TopPicks,
                    SeeAllSection::Featured,
                ] {
                    let is_current = s == section;
                    if store_subtab(ui, s.title(), is_current).clicked() {
                        self.see_all_section = Some(s);
                    }
                    ui.add_space(6.0);
                }
            });
        });

        ui.add_space(24.0);

        let mut action = None;
        let ratings = [
            "9.8", "9.5", "9.2", "9.6", "9.0", "9.4", "8.9", "9.3", "8.7", "9.1",
        ];

        // ── Full Grid of All Games ───────────────────────────────────────────────────
        let card_w = 160.0;
        let spacing_x = 14.0;
        let spacing_y = 16.0;
        let total_avail = ui.available_width();
        let cols = ((total_avail + spacing_x) / (card_w + spacing_x))
            .floor()
            .max(1.0) as usize;

        for chunk in games.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = spacing_x;
                for (chunk_idx, capsule) in chunk.iter().enumerate() {
                    let rating = ratings[(capsule.app_id as usize + chunk_idx) % ratings.len()];
                    if store_poster_card_v2(ui, capsule, card_w, rating, false) {
                        action = Some(StoreAction::Details(capsule.app_id));
                    }
                }
            });
            ui.add_space(spacing_y);
        }

        ui.add_space(32.0);

        match action {
            Some(StoreAction::Details(app_id)) => self.open_details(app_id),
            Some(StoreAction::Activate(app_id)) => self.go_to_activation(app_id),
            None => {}
        }
    }
}
