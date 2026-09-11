
use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;
use crate::ui::components::*;


/// Whether a catalog app passes the active Home filters. Both filters must pass (AND). The lookups
/// are the ones [`DrydockApp::rebuild_repack_index`] / [`DrydockApp::rebuild_fix_index`] maintain.
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
    // The card fills the top of the slot; the bottom LIST_ROW_GAP is left empty so rows separate the
    // same way the Featured cards do. Keeping the allocated slot at `row_height` means callers and
    // `show_rows` still advance by exactly one row.
    // The card fills the whole slot; the gap between rows comes from the caller's `item_spacing.y`.
    let rect = slot;
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    // A persistent card (not just a hover tint) so the gap between rows actually reads — transparent
    // rows on a flat panel show no visible spacing however big the gap is.
    let fill = if selected {
        lerp_color(SURFACE_RAISED, ACCENT, 0.22)
    } else {
        lerp_color(SURFACE, SURFACE_RAISED, 0.35 + hover * 0.65)
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        Stroke::new(
            1.0,
            lerp_color(BORDER, ACCENT, if selected { 0.6 } else { hover }),
        ),
        egui::StrokeKind::Inside,
    );

    let pad = 8.0;
    let img_h = (rect.height() - pad * 2.0).max(1.0);
    let img_w = img_h / STEAM_HEADER_ASPECT;
    let img_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad, rect.top() + pad),
        Vec2::new(img_w, img_h),
    );
    // Only touch artwork for rows near the viewport; off-screen rows draw a neutral tile.
    if ui.clip_rect().expand(row_height * 8.0).intersects(slot) {
        // Cheap App-ID CDN guesses first (header → capsule → library) — most games render here with
        // no API call at all. The `appdetails`-resolved URL is appended only as a last resort and is
        // requested only when the guesses actually fail, so a browse/search never fires a burst of
        // `appdetails` calls that could trip Steam's per-IP rate limit.
        let guesses = steam_artwork_urls(entry.app_id);
        let resolved = headers.get(entry.app_id);
        let mut refs: Vec<&str> = guesses.iter().map(String::as_str).collect();
        if let Some(url) = resolved.as_deref() {
            refs.push(url);
        }
        // Cover-fit so a non-header aspect (a capsule fallback) fills the thumb with no bars.
        let all_failed = paint_remote_image_cover_multi(ui, img_rect, &refs, egui::CornerRadius::same(6));
        if all_failed {
            headers.request(entry.app_id);
        }
    } else {
        ui.painter()
            .rect_filled(img_rect, egui::CornerRadius::same(6), SURFACE);
    }

    // Clip text to the row so a very long name can never bleed into neighbours.
    let text_x = img_rect.right() + 12.0;
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        egui::pos2(text_x, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        &entry.name,
        FontId::proportional(14.0),
        TEXT,
    );
    painter.text(
        egui::pos2(text_x, rect.center().y + 9.0),
        egui::Align2::LEFT_CENTER,
        format!("APP {}", entry.app_id),
        FontId::proportional(10.0),
        MUTED,
    );

    response.clicked()
}

/// Whether an installed app is a real game worth surfacing in the Home library, rather than one of
/// Steam's own runtimes/redistributables that show up as installed "apps".


/// A card-styled filter dropdown: a small uppercase caption above a bordered combo box of fixed
/// `width`, so the Home filters match the surrounding cards instead of egui's default widget look.
pub fn filter_dropdown(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    selected: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(9.0).strong().color(MUTED));
        ui.add_space(5.0);
        // Restyle the combo (button + popup) to the app's card palette for this widget only.
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
            .selected_text(RichText::new(selected).size(12.0).color(TEXT))
            .width(width)
            .show_ui(ui, add_contents);
    });
}

/// What a storefront capsule/hero click asks the Store page to do (applied after the feed borrow).

/// A Steam-style store sub-tab: a pill that fills in when active, sized to its label.
pub fn store_subtab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, TEXT);
    let size = Vec2::new(galley.size().x + 26.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        SURFACE_RAISED
    } else {
        lerp_color(Color32::TRANSPARENT, SURFACE, hover)
    };
    let stroke = if active {
        Stroke::new(1.0, BORDER)
    } else {
        Stroke::NONE
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius {
            nw: 7,
            ne: 7,
            sw: 0,
            se: 0,
        },
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let color = if active {
        Color32::WHITE
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The full-width Featured banner: a wide cinematic strip for the #1 seller with a left-anchored
/// title/price/CTA block over a left-darkening gradient, in the style of Steam's store carousel.
pub fn store_banner(
    ui: &mut egui::Ui,
    capsule: &StoreCapsule,
    rank: usize,
    activatable: bool,
) -> Option<StoreAction> {
    let mut action = None;
    let width = ui.available_width();
    // Height follows the hero image's aspect so `library_hero.jpg` fills the banner exactly — no
    // letterbox bars — and the banner is as wide as the (column-capped) image.
    let height = width / STORE_HERO_ASPECT;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let corner = egui::CornerRadius::same(12);
    // Use the widest capsule art Steam has for this title, falling back to the feed's header.
    let urls = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
            capsule.app_id
        ),
        capsule.header_image_url.clone(),
    ];
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    // Cover-fit (fill + crop) so any art fills the banner edge to edge instead of leaving bars.
    paint_remote_image_cover_multi(ui, rect, &refs, corner);
    let response = ui.interact(rect, ui.id().with(("banner", capsule.app_id)), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let painter = ui.painter().with_clip_rect(rect);
    // Left→right darkening *gradient* so the copy stays legible over any hero art. A soft fade to
    // transparent (not a hard-edged rectangle) avoids a visible seam on evenly-lit art.
    let fade_w = width * 0.70;
    let strips = 48;
    for i in 0..strips {
        let t = i as f32 / (strips - 1) as f32;
        let alpha = (170.0 * (1.0 - t).powf(1.3)) as u8;
        if alpha == 0 {
            continue;
        }
        let x0 = rect.left() + fade_w * (i as f32 / strips as f32);
        let x1 = rect.left() + fade_w * ((i + 1) as f32 / strips as f32);
        let round = if i == 0 {
            egui::CornerRadius {
                nw: 12,
                sw: 12,
                ne: 0,
                se: 0,
            }
        } else {
            egui::CornerRadius::ZERO
        };
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            round,
            Color32::from_rgba_unmultiplied(8, 12, 18, alpha),
        );
    }
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
        egui::StrokeKind::Inside,
    );
    let left = rect.left() + 30.0;
    // Rank + Activatable eyebrow line.
    let mut eyebrow = format!("#{rank} TOP SELLER");
    if activatable {
        eyebrow.push_str("   ·   ACTIVATABLE IN DRYDOCK");
    }
    painter.text(
        egui::pos2(left, rect.top() + 40.0),
        egui::Align2::LEFT_TOP,
        eyebrow,
        FontId::monospace(11.0),
        if activatable { ACCENT_SOFT } else { MUTED },
    );
    painter.text(
        egui::pos2(left, rect.top() + 60.0),
        egui::Align2::LEFT_TOP,
        &capsule.name,
        FontId::proportional(34.0),
        Color32::WHITE,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        action = Some(StoreAction::Details(capsule.app_id));
    }
    // The CTA button, anchored bottom-left.
    let (label, primary) = if activatable {
        ("ACTIVATE IN DRYDOCK", true)
    } else {
        ("VIEW IN STORE", false)
    };
    let btn_w = if activatable { 188.0 } else { 150.0 };
    let btn_rect = egui::Rect::from_min_size(egui::pos2(left, rect.bottom() - 52.0), Vec2::new(btn_w, 34.0));
    let button = if primary {
        success_button(label).min_size(Vec2::new(btn_w, 34.0))
    } else {
        ghost_button(label).min_size(Vec2::new(btn_w, 34.0))
    };
    if ui.put(btn_rect, button).clicked() {
        action = Some(if activatable {
            StoreAction::Activate(capsule.app_id)
        } else {
            StoreAction::Details(capsule.app_id)
        });
    }
    action
}

/// Height of a Steam-style list row (thumbnail + title + meta + a right-hand control).
pub const LIST_ROW_HEIGHT: f32 = 62.0;
/// Transparent gap baked below each row so a visible space always separates the cards, independent
/// of layout item-spacing (which the right-hand `ui.put` control otherwise swallows).
pub const LIST_ROW_GAP: f32 = 5.0;

/// Draws the shared chrome of a Steam-style list row — a full-width band with a small landscape
/// thumbnail, a title and a meta line — and returns the rect reserved on the right for a control
/// plus a click response for the rest of the row (used to open details). `right_w` is the width to
/// reserve on the right; pass 0.0 for a row with no control.
pub fn list_row_base(
    ui: &mut egui::Ui,
    app_id: u32,
    preferred_thumb: Option<&str>,
    title: &str,
    meta: &str,
    meta_color: Color32,
    right_w: f32,
) -> (egui::Rect, egui::Response) {
    // The card fills the top of its slot; the caller wraps each row in a fixed-height region that
    // reserves LIST_ROW_GAP below, so a visible gap always separates the cards.
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
    // Small landscape thumbnail, vertically centred. A caller-supplied URL (the live feed's own,
    // already-valid capsule) is tried first, then the App-ID-derived CDN fallbacks.
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
    // Cover-fit so the thumbnail fills its rect with no letterbox bars — the feed's capsule art is a
    // wider aspect than the header-shaped slot, which a plain fit would bar top and bottom.
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
            FontId::proportional(11.0),
            meta_color,
        );
    }

    let right_zone = egui::Rect::from_min_size(
        egui::pos2(rect.right() - right_w - 12.0, rect.center().y - 16.0),
        Vec2::new(right_w, 32.0),
    );
    // The clickable area is everything left of the control (so the control gets its own clicks).
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

/// A Steam-style store list row: thumbnail + title, an "Activatable in Drydock" meta line when Drydock
/// supports it, and a right-hand Activate/View control. No price (the store list is price-free).
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
    // A fixed-height slot for the card, then an explicit gap below it. egui shrinks a nested
    // `allocate_ui` back to its content, so the gap can't be baked into the slot height — it has to
    // be added as real space after the row for the cards to separate.
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
    // The real gap between cards (the slot itself shrinks to the card, so this must be explicit).
    ui.add_space(LIST_ROW_GAP);
    action
}

/// Runs `contents` inside a fixed-height row slot (`LIST_ROW_HEIGHT`); the caller adds the gap below.
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

/// A successfully detected "Add game to Drydock" result: the game, its resolved install root and the
/// launch `.exe` found inside it.

impl DrydockApp {
    pub fn home_page(&mut self, ui: &mut egui::Ui) {
        if !self.search.trim().is_empty() {
            self.store_search_results(ui);
            return;
        }
        self.start_featured();
        // The shell already caps every page to `CONTENT_WIDTH` (the hero image's width) and centres
        // it, so the banner fills with no side bars and the list lines up under it — no extra column
        // needed here.
        self.home_store_column(ui);
    }

    /// The storefront column body: sub-tabs, divider and the selected tab's content.
    pub fn home_store_column(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        // Steam-style store sub-tabs.
        let mut selected = self.store_tab;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (tab, label) in StoreTab::ALL {
                if store_subtab(ui, label, selected == tab).clicked() {
                    selected = tab;
                }
            }
        });
        if selected != self.store_tab {
            self.store_tab = selected;
            // Same reasoning as the page switch: the previous tab's rows are gone, so its queued
            // header lookups should not keep consuming the shared Steam request budget.
            self.header_resolver.cancel_pending();
        }
        ui.add_space(2.0);
        let line_y = ui.cursor().top();
        ui.painter()
            .hline(ui.max_rect().x_range(), line_y, Stroke::new(1.0, BORDER));
        ui.add_space(18.0);

        // The whole storefront reads from an owned snapshot so the render helpers can borrow `self`
        // immutably for the Activatable check without fighting the feed borrow.
        let featured = self.featured.clone();
        let action = match self.store_tab {
            StoreTab::Featured => self.store_featured(ui, featured.as_ref()),
            StoreTab::NewReleases => {
                self.store_shelf(ui, featured.as_ref().map(|feed| feed.new_releases.as_slice()))
            }
            StoreTab::Repacks => self.store_repacks(ui),
            StoreTab::DenuvoWatch => self.store_denuvo_watch(ui),
        };
        match action {
            Some(StoreAction::Details(app_id)) => self.open_details(app_id),
            Some(StoreAction::Activate(app_id)) => self.go_to_activation(app_id),
            None => {}
        }
    }

    /// A small "loading / offline" line shared by the storefront tabs while the live feed resolves.
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
                        .size(12.5)
                        .color(MUTED),
                );
            });
        } else if let Some(error) = &self.featured_error {
            ui.label(
                RichText::new(format!("Steam storefront is unavailable: {error}"))
                    .size(12.5)
                    .color(AMBER),
            );
        }
        false
    }

    /// The Featured tab, laid out like Steam's store landing: a wide cinematic banner for the #1 top
    /// seller, then an "Featured & Recommended" grid of the rest, each with its Activatable overlay.
    pub fn store_featured(&self, ui: &mut egui::Ui, featured: Option<&StoreFeatured>) -> Option<StoreAction> {
        if !self.store_feed_status(ui) {
            return None;
        }
        let feed = featured?;
        if self.catalog.is_empty() {
            ui.label(RichText::new("Loading the game list…").color(MUTED));
            return None;
        }
        // Steam's ranked top sellers, narrowed to the games actually in our catalog (proxy gamelist),
        // so every card is one Drydock can get.
        let available: Vec<&StoreCapsule> = feed
            .top_sellers
            .iter()
            .filter(|capsule| self.catalog_ids.contains(&capsule.app_id))
            .collect();
        let mut items = available.into_iter();
        let Some(hero) = items.next() else {
            ui.label(
                RichText::new("None of Steam's top sellers are available in Drydock right now.").color(MUTED),
            );
            return None;
        };
        let mut action = None;
        // Full-width cinematic banner for the headline title. The storefront only ever offers "View"
        // — Activate lives on the details page, driven by the game's real Denuvo status.
        if let Some(hit) = store_banner(ui, hero, 1, false) {
            action = Some(hit);
        }

        ui.add_space(22.0);
        section_label(ui, "FEATURED & RECOMMENDED");
        ui.add_space(12.0);
        // The rest of the available top sellers as a price-free Steam-style list.
        ui.spacing_mut().item_spacing.y = 0.0;
        for capsule in items.take(20) {
            if let Some(hit) = store_list_row(ui, capsule, false) {
                action = Some(hit);
            }
        }
        action
    }

    /// A store category tab (New Releases): the live category as a price-free Steam-style list of
    /// rows.
    pub fn store_shelf(&self, ui: &mut egui::Ui, capsules: Option<&[StoreCapsule]>) -> Option<StoreAction> {
        if !self.store_feed_status(ui) {
            return None;
        }
        let capsules = capsules.unwrap_or_default();
        if capsules.is_empty() {
            ui.label(RichText::new("Nothing to show in this category right now.").color(MUTED));
            return None;
        }
        // Narrow the category to games actually in our catalog (proxy gamelist), so every row is
        // one Drydock can get.
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
        // No nested scroll area — the page's own scroll handles overflow (a scroll area inside the
        // centred zero-height content column collapses and clips the list).
        ui.spacing_mut().item_spacing.y = 0.0;
        for capsule in available {
            // Storefront rows are always "View"; Activate lives on the details page.
            if let Some(hit) = store_list_row(ui, capsule, false) {
                action = Some(hit);
            }
        }
        ui.add_space(24.0);
        action
    }

    /// The Drydock-only Denuvo Watch tab: the live set of games that actually use Denuvo — i.e. the
    /// ones that need Drydock to activate them — as a searchable list. Independent of the Steam feed.
    pub fn store_denuvo_watch(&self, ui: &mut egui::Ui) -> Option<StoreAction> {
        if !self.denuvo_loaded {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Loading the Denuvo watch list…")
                        .size(12.5)
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
            .size(12.5)
            .color(MUTED),
        );
        ui.add_space(14.0);
        // Only Denuvo games we can name from the catalogue, alphabetised.
        let mut games: Vec<&CatalogApp> = self
            .catalog
            .iter()
            .filter(|entry| self.denuvo_appids.contains(&entry.app_id))
            .collect();
        games.sort_by_key(|entry| entry.name.to_lowercase());

        let mut action = None;
        // Same row structure as the Featured list — a fixed-height slot plus an explicit gap below —
        // so the spacing between cards is identical (a bare `item_spacing` doesn't take here).
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

    /// The Repacks tab: every catalogue game Drydock has an external repack download for, as a
    /// searchable list. Independent of the Steam feed.
    pub fn store_repacks(&self, ui: &mut egui::Ui) -> Option<StoreAction> {
        if !self.repacks_loaded {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(RichText::new("Loading the repack list…").size(12.5).color(MUTED));
            });
            return None;
        }
        // Only games we can name from the catalogue that have at least one repack source.
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
            .size(12.5)
            .color(MUTED),
        );
        ui.add_space(14.0);

        let mut action = None;
        // Same row structure as the Featured list (fixed slot + explicit gap) for identical spacing.
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

    /// The nav-search results view: the whole catalogue filtered by the query and the repack/fix
    /// dropdowns, as a list of rows that open the details page.
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
                .size(11.0)
                .color(MUTED),
            );
            ui.add_space(8.0);
            let mut open = None;
            if matches.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("No games match your search.").color(MUTED));
                ui.add_space(6.0);
            }
            // Same row structure as the Featured list (fixed slot + explicit gap below) for identical
            // card spacing.
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

    /// The Library page: unlike the Store (which lists every supported game), this shows only the
    /// user's own collection — games Steam has installed plus games activated through Drydock — each as
    /// a capsule with a Play button. A game activated outside Steam (no manifest) can be pointed at
    /// its own `.exe` so it launches from here too, just like a Steam-installed game.
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
                // Align the Clear link with the dropdown boxes (a label sits above those).
                ui.vertical(|ui| {
                    ui.add_space(17.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("✕  Clear").size(11.0).color(ACCENT))
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
}

