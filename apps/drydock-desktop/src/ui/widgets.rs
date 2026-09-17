use crate::ui::helpers::*;
use crate::ui::pages::home::search_result_row;
use crate::ui::theme::*;
use crate::ui::types::*;
use drydock_core::*;
use egui::{Color32, FontId, RichText, Sense, Stroke, Vec2};

pub fn back_button(ui: &mut egui::Ui, tooltip: &str) -> egui::Response {
    let size = Vec2::new(34.0, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = lerp_color(SURFACE, SURFACE_RAISED, hover);
    let stroke = Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover));
    ui.painter().rect(rect, 9, fill, stroke, egui::StrokeKind::Inside);
    let center = rect.center();
    let color = lerp_color(TEXT, Color32::WHITE, hover);
    let dx = 4.0;
    ui.painter().add(egui::Shape::line(
        vec![
            egui::pos2(center.x + dx, center.y - 8.0),
            egui::pos2(center.x - dx, center.y),
            egui::pos2(center.x + dx, center.y + 8.0),
        ],
        Stroke::new(2.2, color),
    ));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(tooltip)
}

/// A screenshot-carousel navigation button that paints a chevron and hovers.
/// A polished screenshot gallery: one large main image with subtle translucent circular arrows and
/// an index badge overlaid on it, plus a clickable thumbnail filmstrip below. Returns the
/// (possibly changed) index. The caller guarantees `screenshots` is non-empty.

#[derive(Clone, Copy)]
pub enum ButtonKind {
    Primary,
    Ghost,
    Success,
}

/// A rounded button that animates its fill/stroke on hover. Implements [`egui::Widget`]
/// so every existing `add`, `add_enabled`, and `add_sized` call site keeps working.
pub struct PillButton {
    pub label: String,
    pub kind: ButtonKind,
    pub min_size: Vec2,
    pub padding: Option<Vec2>,
}

impl PillButton {
    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }

    pub fn compact(mut self) -> Self {
        self.padding = Some(Vec2::new(8.0, 5.0));
        self
    }
}

pub fn primary_button(label: &str) -> PillButton {
    PillButton {
        label: label.to_owned(),
        kind: ButtonKind::Primary,
        min_size: Vec2::ZERO,
        padding: None,
    }
}

pub fn ghost_button(label: &str) -> PillButton {
    PillButton {
        label: label.to_owned(),
        kind: ButtonKind::Ghost,
        min_size: Vec2::ZERO,
        padding: None,
    }
}

/// A Steam-green call-to-action button, used for the Library's Play control.
pub fn success_button(label: &str) -> PillButton {
    PillButton {
        label: label.to_owned(),
        kind: ButtonKind::Success,
        min_size: Vec2::ZERO,
        padding: None,
    }
}

impl egui::Widget for PillButton {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let enabled = ui.is_enabled();
        let font = FontId::proportional(14.0);
        let galley = ui
            .painter()
            .layout_no_wrap(self.label.clone(), font.clone(), Color32::WHITE);
        let padding = self.padding.unwrap_or_else(|| Vec2::new(18.0, 9.0));
        let mut size = (galley.size() + 2.0 * padding).max(self.min_size);
        size.y = size.y.max(36.0);
        // Fill the allocated box when placed by `add_sized` (a justified layout).
        let layout = *ui.layout();
        if layout.main_justify || layout.cross_justify {
            size = size.max(ui.available_size_before_wrap());
        }
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());

        let hover = if enabled {
            ui.ctx().animate_bool(response.id, response.hovered())
        } else {
            0.0
        };
        let (fill, stroke, text_color) = match self.kind {
            ButtonKind::Primary => (
                lerp_color(ACCENT, ACCENT_SOFT, hover),
                Stroke::new(1.2, lerp_color(ACCENT, Color32::WHITE, hover)),
                Color32::from_rgb(4, 14, 24),
            ),
            ButtonKind::Ghost => (
                lerp_color(SURFACE, SURFACE_RAISED, hover),
                Stroke::new(1.0, lerp_color(BORDER, ACCENT, hover)),
                lerp_color(TEXT, ACCENT_SOFT, hover),
            ),
            ButtonKind::Success => (
                lerp_color(VERDIGRIS, Color32::from_rgb(80, 255, 210), hover),
                Stroke::new(1.2, lerp_color(VERDIGRIS, Color32::WHITE, hover)),
                Color32::from_rgb(4, 20, 15),
            ),
        };
        let (fill, stroke, text_color) = if enabled {
            (fill, stroke, text_color)
        } else {
            (
                lerp_color(fill, BACKGROUND, 0.45),
                Stroke::new(stroke.width, lerp_color(stroke.color, BACKGROUND, 0.5)),
                MUTED,
            )
        };

        if ui.is_rect_visible(rect) {
            ui.painter()
                .rect(rect, 10, fill, stroke, egui::StrokeKind::Inside);
            let galley = ui.painter().layout_no_wrap(self.label, font, text_color);
            let pos = rect.center() - galley.size() / 2.0;
            ui.painter().galley(pos, galley, text_color);
        }
        if enabled && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }
}

pub fn page_heading(ui: &mut egui::Ui, title: &str) {
    ui.add(egui::Label::new(RichText::new(title).size(30.0).strong().color(TEXT)).wrap());
}

/// A dynamic game search: a heading-sized search field over a fixed-height results list. Returns the
/// App ID the user clicked, if any. The results box is a constant height (never resizing with the
/// match count), so neither it nor its scrollbar jumps as the query changes. When the query already
/// equals the selected game's name the list stays collapsed (the caller has its pick).
#[allow(clippy::too_many_arguments)]
pub fn game_search_box(
    ui: &mut egui::Ui,
    id: &str,
    search: &mut String,
    selected: Option<u32>,
    catalog: &[CatalogApp],
    headers: &HeaderResolver,
    width: f32,
    // When true (a filter is active) the results stay open even with an empty query, so the user can
    // browse everything the filter matches. `extra_filter` narrows the catalog before the text match.
    allow_empty: bool,
    extra_filter: impl Fn(&CatalogApp) -> bool,
) -> Option<u32> {
    let mut clicked = None;
    // Natural height with a slightly heavier top margin so the text sits optically centred (egui
    // top-aligns the galley within the line box, which otherwise reads a touch high).
    ui.add(
        egui::TextEdit::singleline(search)
            .hint_text("Search by name or App ID")
            .font(egui::TextStyle::Heading)
            .margin(egui::Margin {
                left: 18,
                right: 18,
                top: 16,
                bottom: 12,
            })
            .desired_width(width),
    );

    let query = search.trim().to_lowercase();
    let selected_name = selected
        .and_then(|app_id| catalog.iter().find(|entry| entry.app_id == app_id))
        .map(|entry| entry.name.to_lowercase());
    // Without an active filter, an empty query (or re-selecting the current pick) collapses the list.
    if !allow_empty && (query.is_empty() || selected_name.as_deref() == Some(query.as_str())) {
        return None;
    }

    let matches: Vec<&CatalogApp> = catalog
        .iter()
        .filter(|entry| {
            extra_filter(entry)
                && (query.is_empty()
                    || entry.name.to_lowercase().contains(&query)
                    || entry.app_id.to_string().contains(&query))
        })
        .take(100)
        .collect();

    ui.add_space(8.0);
    egui::Frame::new()
        .fill(SURFACE_RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(8)
        .show(ui, |ui| {
            // Subtract the frame's inner margin (8px each side) so the panel's OUTER width matches
            // the search field above it exactly, instead of overhanging by the margins.
            ui.set_width((width - 16.0).max(1.0));
            ui.set_height(360.0);
            if matches.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new("No matching games").size(14.0).color(MUTED));
                    });
            } else {
                // Fixed-height rows via `show_rows` so only the visible results are laid out — this
                // keeps the header-image thumbnails to the handful on screen instead of fetching one
                // per match.
                let row_height = 50.0;
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, matches.len(), |ui, range| {
                        for index in range {
                            let entry = matches[index];
                            if search_result_row(
                                ui,
                                entry,
                                row_height,
                                selected == Some(entry.app_id),
                                headers,
                            ) {
                                clicked = Some(entry.app_id);
                            }
                        }
                    });
            }
        });
    clicked
}

pub fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(14.0).strong().color(ACCENT));
}

pub fn panel(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(16)
        .inner_margin(22)
        .show(ui, |ui| {
            // Fill the container width so stacked panels line up instead of shrinking to content.
            ui.set_width(ui.available_width());
            content(ui);
        });
}

pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32, warning: bool) {
    egui::Frame::new()
        .fill(lerp_color(SURFACE, color, 0.18))
        .stroke(Stroke::new(1.0, lerp_color(BORDER, color, 0.55)))
        .corner_radius(255)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if warning {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(13.0, 12.0), Sense::hover());
                    draw_warning_triangle(ui.painter(), rect, color);
                }
                ui.label(RichText::new(text).size(14.0).strong().color(color));
            });
        });
}

pub fn status_pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    pill(ui, text, color, false);
}

/// An amber "activation needed" chip with a drawn warning triangle, for DRM-protected games.
/// Paints a small warning triangle with an exclamation mark inside `rect`.
pub fn draw_warning_triangle(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = Stroke::new(1.3, color);
    let top = egui::pos2(rect.center().x, rect.top());
    let bottom_left = egui::pos2(rect.left(), rect.bottom());
    let bottom_right = egui::pos2(rect.right(), rect.bottom());
    painter.add(egui::Shape::closed_line(
        vec![top, bottom_left, bottom_right],
        stroke,
    ));
    let cx = rect.center().x;
    painter.line_segment(
        [
            egui::pos2(cx, rect.top() + rect.height() * 0.38),
            egui::pos2(cx, rect.top() + rect.height() * 0.66),
        ],
        stroke,
    );
    painter.circle_filled(egui::pos2(cx, rect.bottom() - rect.height() * 0.13), 0.9, color);
}

/// A modern sliding toggle switch. Returns a response whose `changed()` fires on flip.
pub fn toggle_switch(ui: &mut egui::Ui, on: &mut bool, accent: Color32) -> egui::Response {
    let size = Vec2::new(40.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, ""));

    let how_on = ui.ctx().animate_bool(response.id, *on);
    let painter = ui.painter();
    let radius = rect.height() / 2.0;
    let track = lerp_color(BORDER, accent, how_on);
    painter.rect_filled(rect, radius, track);
    let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
    painter.circle_filled(egui::pos2(knob_x, rect.center().y), radius - 3.0, Color32::WHITE);
    response
}

/// Paints a remote image into `rect`. Returns true if the image failed to load.
pub fn paint_remote_image(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    uri: &str,
    corner: egui::CornerRadius,
    fallback: &str,
) -> bool {
    ui.painter().rect_filled(rect, corner, SURFACE);
    let texture = ui.ctx().try_load_texture(
        uri,
        egui::TextureOptions::LINEAR,
        egui::load::SizeHint::Width(rect.width().max(1.0) as u32),
    );
    match texture {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            ui.put(
                rect,
                egui::Image::from_texture(texture)
                    .fit_to_exact_size(rect.size())
                    .corner_radius(corner),
            );
            false
        }
        Ok(egui::load::TexturePoll::Pending { .. }) => {
            // No spinner — the neutral tile stays until the art loads in the background.
            false
        }
        Err(_) => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                fallback,
                FontId::monospace(14.0),
                MUTED,
            );
            true
        }
    }
}

/// Paints a remote image so it *covers* `rect` — scaled up until it fills the whole rectangle, with
/// any overflow cropped (via a centred UV window) rather than letterboxed. Keeps the rounded corners.
/// Used for the screenshot carousel so the preview never shows side/top bars regardless of the
/// image's exact aspect ratio.
pub fn paint_remote_image_cover(ui: &mut egui::Ui, rect: egui::Rect, uri: &str, corner: egui::CornerRadius) {
    ui.painter().rect_filled(rect, corner, SURFACE);
    let texture = ui.ctx().try_load_texture(
        uri,
        egui::TextureOptions::LINEAR,
        egui::load::SizeHint::Width(rect.width().max(1.0) as u32),
    );
    match texture {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            let img = texture.size;
            let img_aspect = if img.y > 0.0 { img.x / img.y } else { 1.0 };
            let rect_aspect = rect.height().max(1.0).recip() * rect.width();
            // Crop the dimension that would otherwise overflow, centred, so the visible window's
            // aspect matches the rect exactly — no distortion, no bars.
            let uv = if rect_aspect > img_aspect {
                let h = (img_aspect / rect_aspect).clamp(0.0, 1.0);
                egui::Rect::from_min_max(egui::pos2(0.0, (1.0 - h) / 2.0), egui::pos2(1.0, (1.0 + h) / 2.0))
            } else {
                let w = (rect_aspect / img_aspect).clamp(0.0, 1.0);
                egui::Rect::from_min_max(egui::pos2((1.0 - w) / 2.0, 0.0), egui::pos2((1.0 + w) / 2.0, 1.0))
            };
            ui.put(
                rect,
                egui::Image::from_texture(texture)
                    .uv(uv)
                    .fit_to_exact_size(rect.size())
                    .maintain_aspect_ratio(false)
                    .corner_radius(corner),
            );
        }
        Ok(egui::load::TexturePoll::Pending { .. }) => {
            // No spinner — the neutral tile stays until the screenshot loads in the background.
        }
        Err(_) => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "SCREENSHOT UNAVAILABLE",
                FontId::monospace(14.0),
                MUTED,
            );
        }
    }
}

/// Like [`paint_remote_image_cover`] but tries several URIs in order, advancing to the next only when
/// one definitively fails to load. Cover-fills the rect (no letterbox bars) — used for the Downloads
/// banner, where the card is much wider than the source art.
pub fn paint_remote_image_cover_multi(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    uris: &[&str],
    corner: egui::CornerRadius,
) -> bool {
    ui.painter().rect_filled(rect, corner, SURFACE);
    for (index, uri) in uris.iter().enumerate() {
        let is_last = index + 1 == uris.len();
        let texture = ui.ctx().try_load_texture(
            uri,
            egui::TextureOptions::LINEAR,
            egui::load::SizeHint::Width(rect.width().max(1.0) as u32),
        );
        match texture {
            Ok(egui::load::TexturePoll::Ready { texture }) => {
                let img = texture.size;
                let img_aspect = if img.y > 0.0 { img.x / img.y } else { 1.0 };
                let rect_aspect = rect.height().max(1.0).recip() * rect.width();
                let uv = if rect_aspect > img_aspect {
                    let h = (img_aspect / rect_aspect).clamp(0.0, 1.0);
                    egui::Rect::from_min_max(
                        egui::pos2(0.0, (1.0 - h) / 2.0),
                        egui::pos2(1.0, (1.0 + h) / 2.0),
                    )
                } else {
                    let w = (rect_aspect / img_aspect).clamp(0.0, 1.0);
                    egui::Rect::from_min_max(
                        egui::pos2((1.0 - w) / 2.0, 0.0),
                        egui::pos2((1.0 + w) / 2.0, 1.0),
                    )
                };
                ui.put(
                    rect,
                    egui::Image::from_texture(texture)
                        .uv(uv)
                        .fit_to_exact_size(rect.size())
                        .maintain_aspect_ratio(false)
                        .corner_radius(corner),
                );
                return false;
            }
            Ok(egui::load::TexturePoll::Pending { .. }) => {
                // No spinner — the neutral tile stays until the art loads in the background.
                return false;
            }
            Err(_) if !is_last => {}
            Err(_) => return true,
        }
    }
    true
}
