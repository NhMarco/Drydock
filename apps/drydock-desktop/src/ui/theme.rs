use crate::ui::helpers::lerp_color;
use egui::{Color32, Stroke, Vec2};
use std::time::Duration;

pub const BACKGROUND: Color32 = Color32::from_rgb(6, 14, 23); // #060E17 Deepest oceanic void
pub const SURFACE: Color32 = Color32::from_rgb(13, 27, 40); // #0D1B28 Dark glass surface
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(18, 36, 52); // #122434 Raised glass container
pub const BORDER: Color32 = Color32::from_rgb(26, 54, 76); // #1A364C Subtle cyan glass border
pub const TEXT: Color32 = Color32::from_rgb(240, 246, 252); // #F0F6FC Crisp bright text
pub const MUTED: Color32 = Color32::from_rgb(139, 161, 179); // #8BA1B3 Muted slate
/// Primary brand accent: neon cyan glow. Active controls, focus rings, primary highlights.
pub const ACCENT: Color32 = Color32::from_rgb(0, 225, 250); // #00E1FA Neon cyan glow
/// Lighter soft cyan for hover highlights, spinners, and badges.
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(118, 242, 255); // #76F2FF Soft cyan
/// Darkened cyan for active pressed states and secondary fills.
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0, 150, 172); // #0096AC Deep cyan
/// Verdigris green — "installed", "ready", "play".
pub const VERDIGRIS: Color32 = Color32::from_rgb(0, 230, 180); // #00E6B4 Verdigris
/// Warning amber for alerts.
pub const AMBER: Color32 = Color32::from_rgb(255, 196, 61); // #FFC43D Amber warning
/// Danger red for destructive actions and errors.
pub const DANGER: Color32 = Color32::from_rgb(255, 82, 82); // #FF5252 Danger red
pub const SIDEBAR_FILL: Color32 = Color32::from_rgb(8, 18, 28); // #08121C Deep sidebar fill
pub const CATALOG_REFRESH_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
/// Steam header art is 460×215 — this ratio is used for card cover heights and detail artwork.
pub const STEAM_HEADER_ASPECT: f32 = 0.467;
/// Height of the top navigation bar and the bottom status strip that frame the storefront.
pub const SIDEBAR_WIDTH: f32 = 256.0;
#[allow(dead_code)]
pub const TOP_NAV_HEIGHT: f32 = 52.0;
pub const STATUS_BAR_HEIGHT: f32 = 42.0;
pub const MIN_CONTENT_GUTTER: f32 = 24.0;
/// Every tab's content is capped to this single width and centred, so all pages share one aligned
/// column and none stretches edge-to-edge on wide monitors; on narrower windows it fills to within
/// `MIN_CONTENT_GUTTER` of each side. It equals the storefront hero image's width, so the Store
/// banner fills edge to edge with no letterbox bars. `STORE_HERO_ASPECT` is Steam's `library_hero.jpg`
/// ratio (1920×620), used to size that banner.
pub const CONTENT_WIDTH: f32 = 1040.0;
pub const STORE_HERO_ASPECT: f32 = 3.1;

pub const UPDATE_CHECK_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// Shortest gap between network-backed Steam Service status checks triggered by page
/// switches, so rapidly flipping tabs does not hammer the payload repository.
pub const SERVICE_RECHECK_COOLDOWN: Duration = Duration::from_secs(30);
/// How often the unlocks of apps added with the latest version are compared with the provider's.
pub const UNLOCK_UPDATE_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// How often the UI looks at whether an unlock update sweep is due.
pub const UNLOCK_UPDATE_POLL: Duration = Duration::from_secs(5 * 60);
/// The pause between two apps in a sweep: each fetch may make the provider build a package.
pub const UNLOCK_UPDATE_SPACING: Duration = Duration::from_secs(2);

/// Lucide Vector Icons Unicode mappings (Private Use Area glyphs from lucide.ttf).
#[allow(dead_code)]
pub mod icons {
    pub const STORE: &str = "\u{e3e4}";
    pub const LIBRARY: &str = "\u{e100}";
    pub const DOWNLOAD: &str = "\u{e0b2}";
    pub const ACTIVATION: &str = "\u{e0fd}";
    pub const TOOLS: &str = "\u{e1b1}";
    pub const CLOUD: &str = "\u{e088}";
    pub const SETTINGS: &str = "\u{e154}";
    pub const UPDATES: &str = "\u{e145}";
    pub const HELP: &str = "\u{e082}";
    pub const SEARCH: &str = "\u{e151}";
    pub const FLAME: &str = "\u{e0d2}";
    pub const STAR: &str = "\u{e176}";
    pub const SHIELD: &str = "\u{e158}";
    pub const CHEVRON_RIGHT: &str = "\u{e06f}";
    pub const CHEVRON_LEFT: &str = "\u{e06e}";
    pub const CHECK: &str = "\u{e06c}";
    pub const CLOSE: &str = "\u{e1b2}";
    pub const PLAY: &str = "\u{e13c}";
    pub const FOLDER: &str = "\u{e0d7}";
    pub const SPARKLES: &str = "\u{e412}";
}

pub fn install_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // 1. Plus Jakarta Sans (Primary font)
    fonts.font_data.insert(
        "jakarta".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/PlusJakartaSans-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        "jakarta-bold".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/PlusJakartaSans-Bold.ttf"
        ))),
    );

    // 2. Lucide Icons (Vector icon glyphs)
    fonts.font_data.insert(
        "lucide".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/lucide.ttf"
        ))),
    );

    // 3. Noto Sans CJK Fallback
    fonts.font_data.insert(
        "noto-cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../../assets/fonts/NotoSansCJKsc-Regular.otf"
        ))),
    );

    // Prioritize Plus Jakarta Sans, then Lucide Icons, then CJK Fallback:
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "noto-cjk".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "lucide".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "jakarta".to_owned());

    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("lucide".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("noto-cjk".to_owned());

    context.set_fonts(fonts);
}

pub fn install_style(context: &egui::Context) {
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(16.0, 10.0);
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = SURFACE;
    style.visuals.window_stroke = Stroke::new(1.0, BORDER);
    style.visuals.window_corner_radius = egui::CornerRadius::same(14);
    style.visuals.extreme_bg_color = Color32::from_rgb(10, 22, 34);
    style.visuals.faint_bg_color = SURFACE_RAISED;
    style.visuals.widgets.inactive.bg_fill = SURFACE_RAISED;
    style.visuals.widgets.inactive.weak_bg_fill = SURFACE_RAISED;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.hovered.bg_fill = SURFACE_RAISED;
    style.visuals.widgets.hovered.weak_bg_fill = SURFACE_RAISED;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.bg_fill = ACCENT_DEEP;
    style.visuals.widgets.active.weak_bg_fill = ACCENT_DEEP;
    style.visuals.selection.bg_fill = lerp_color(BACKGROUND, ACCENT_DEEP, 0.45);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_SOFT);
    style.visuals.text_cursor.stroke = Stroke::new(2.0, ACCENT);

    // Soften every widget's corners for a consistent glassmorphic look.
    let radius = egui::CornerRadius::same(10);
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
    }

    // Ensure smallest font size is at least 14px so text is never too small
    for font_id in style.text_styles.values_mut() {
        if font_id.size < 14.0 {
            font_id.size = 14.0;
        }
    }

    // Thin, floating scrollbars with cyan glow handle on hover.
    let mut scroll = egui::style::ScrollStyle::floating();
    scroll.bar_width = 8.0;
    scroll.floating_width = 8.0;
    scroll.floating_allocated_width = 0.0;
    scroll.handle_min_length = 32.0;
    scroll.bar_inner_margin = 2.0;
    scroll.foreground_color = true;
    scroll.dormant_handle_opacity = 0.45;
    scroll.interact_handle_opacity = 1.0;
    scroll.active_handle_opacity = 1.0;
    scroll.dormant_background_opacity = 0.0;
    scroll.interact_background_opacity = 0.0;
    scroll.active_background_opacity = 0.0;
    style.spacing.scroll = scroll;

    context.set_style_of(egui::Theme::Dark, style);
    context.options_mut(|options| {
        options.theme_preference = egui::ThemePreference::Dark;
        options.fallback_theme = egui::Theme::Dark;
    });
}

pub fn paint_backdrop(ui: &mut egui::Ui, seconds: f32) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let drift_x = (seconds * 0.18).sin() * 30.0;
    let drift_y = (seconds * 0.12).cos() * 20.0;

    // Ambient oceanic void glows: electric cyan top-center, verdigris bottom-left, deep cyan top-right.
    painter.circle_filled(
        egui::pos2(rect.center().x + drift_x, rect.top() + 60.0 + drift_y),
        260.0,
        Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), 8),
    );
    painter.circle_filled(
        egui::pos2(rect.right() - 120.0 - drift_x, rect.top() + 140.0),
        200.0,
        Color32::from_rgba_unmultiplied(ACCENT_DEEP.r(), ACCENT_DEEP.g(), ACCENT_DEEP.b(), 12),
    );
    painter.circle_filled(
        egui::pos2(rect.left() + 200.0 + drift_x, rect.bottom() - 80.0),
        220.0,
        Color32::from_rgba_unmultiplied(VERDIGRIS.r(), VERDIGRIS.g(), VERDIGRIS.b(), 10),
    );
}
