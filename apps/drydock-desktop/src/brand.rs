//! What this build looks like: its name, its colours, where its navigation sits, and which optional
//! features it offers.
//!
//! Drydock can be built as a white-label product — the same program with another name and look —
//! by adding a `brand/` folder with a `brand.toml` (docs/branding.md). `build.rs` turns that file
//! into the [`BRAND`] this module includes; without one, [`BRAND`] is [`DRYDOCK`]. The rest of the
//! app never asks which product it is: it takes its colours from [`BRAND.palette`](Brand::palette),
//! lays its navigation out by [`BRAND.navigation`](Brand::navigation) and checks
//! [`BRAND.features`](Brand::features) before it offers something optional. That is what keeps a
//! white-label repository nothing but Drydock plus its `brand/` folder, updated with a plain pull.

use eframe::egui::{self, Color32};

/// Every colour the interface paints with. Nothing in the UI uses a colour literal of its own, so a
/// brand's palette re-skins all of it.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// The page ground behind everything.
    pub background: Color32,
    /// Panels and cards.
    pub surface: Color32,
    /// Hovered and raised elements on a surface.
    pub surface_raised: Color32,
    /// Inset areas: code blocks, log views, response fields.
    pub surface_sunken: Color32,
    pub border: Color32,
    /// The hairline between the navigation or status bar and the page.
    pub edge: Color32,
    pub text: Color32,
    pub muted: Color32,
    /// The product colour: primary buttons, the active navigation entry, focus.
    pub accent: Color32,
    /// A lighter accent for secondary emphasis and inline highlights.
    pub accent_soft: Color32,
    /// A darker accent for badges, selections and gradients' far stop.
    pub accent_deep: Color32,
    /// "Installed", "ready", "play".
    pub success: Color32,
    pub success_hover: Color32,
    /// Text on a success-coloured button.
    pub on_success: Color32,
    /// Warnings; always paired with a ⚠ glyph so it never carries meaning alone.
    pub warning: Color32,
    /// Destructive actions and errors.
    pub danger: Color32,
    /// The navigation and status bars.
    pub chrome: Color32,
    /// The colour dark scrims over artwork are made of (their opacity is chosen where they are used).
    pub scrim: Color32,
    /// The full-window overlay shown while a blocking action runs.
    pub overlay: Color32,
    /// Text-field backgrounds.
    pub input: Color32,
    /// The second, slower glow drifting behind the pages; the first is the accent.
    pub ambient: Color32,
}

/// Where the main navigation sits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "a build uses one layout, so the other is never constructed"
)]
pub enum Navigation {
    /// A storefront bar across the top of the window.
    Top,
    /// A column down the left edge, logo at the head.
    Side,
}

/// The optional features. Everything else — Store, Library, Activation, Downloads, Settings,
/// Help — is the core of every product and is always there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Features {
    /// The Tools page: game-language switcher and the local emulator cracker.
    pub tools: bool,
    /// The Cloud page: cloud save redirection and its providers.
    pub cloud: bool,
    /// The Store's Denuvo tab, listing the games that ship with Denuvo. (The Denuvo filter and
    /// badges stay either way: they come from the same list, which the Store needs regardless.)
    pub denuvo_tab: bool,
    /// The Repacks tab, repack filters, and "Download repack" on game pages.
    pub repacks: bool,
    /// "Apply Denuvo fix" on game pages, the Denuvo-fix filter and its walkthrough.
    pub denuvo_fix: bool,
    /// "Add cracked version to Steam" (the build-locked unlock that goes with a Denuvo fix).
    pub cracked_version: bool,
}

impl Features {
    /// Whether anything needs the Denuvo-fix list at all; without it the app never fetches it.
    #[must_use]
    pub const fn uses_fix_list(self) -> bool {
        self.denuvo_fix || self.cracked_version
    }
}

/// One product's look.
#[derive(Clone, Copy, Debug)]
pub struct Brand {
    /// The name shown to people. Comes from the core, which also names the files after it.
    pub name: &'static str,
    /// A short line under the name where there is room (the side navigation's head).
    pub tagline: &'static str,
    /// Whether the logo already spells the name (and tagline), so the side navigation shows the
    /// logo alone instead of repeating them beneath it.
    pub logo_is_wordmark: bool,
    /// The product's Discord invite (`https://discord.gg/…`), offered as a button in the navigation
    /// and after an activation. `None` shows no button. Never inherited from Drydock: a product
    /// that names no server of its own must not send its users to somebody else's.
    pub discord: Option<&'static str>,
    /// Who made the product, credited in the About dialog ("Developed by …").
    pub developer: &'static str,
    pub palette: Palette,
    pub navigation: Navigation,
    pub features: Features,
}

/// Drydock: oxidised metals in a shipyard, not a storefront.
///
/// The first scheme was Steam's own — navy grounds with `#66C0F4`, literally Valve's brand blue —
/// and made the app read as a Steam product. This one is built from what a dry dock is made of:
/// brass fittings, copper gone to verdigris, iron gone to rust, over a cold slate ground. The
/// neutrals carry a faint warm bias so they sit under the brass instead of fighting it, and the three
/// semantic colours are separated by hue *and* lightness so state stays readable without colour.
///
/// Also the defaults of every white-label brand: whatever its `brand.toml` leaves out is Drydock's.
#[allow(
    dead_code,
    reason = "a brand that sets every value of its own never falls back to Drydock's"
)]
pub const DRYDOCK: Brand = Brand {
    name: drydock_core::brand::DRYDOCK.name,
    tagline: "Download · Activate · Play",
    logo_is_wordmark: false,
    discord: None,
    developer: "Drydock contributors",
    palette: Palette {
        background: Color32::from_rgb(21, 24, 27),     // #15181B cold slate
        surface: Color32::from_rgb(30, 35, 40),        // #1E2328
        surface_raised: Color32::from_rgb(40, 47, 54), // #282F36
        surface_sunken: Color32::from_rgb(18, 21, 24), // #121518
        border: Color32::from_rgb(56, 66, 76),         // #38424C
        edge: Color32::from_rgb(13, 15, 17),           // #0D0F11
        text: Color32::from_rgb(232, 230, 227),        // #E8E6E3 warm off-white
        muted: Color32::from_rgb(142, 146, 153),       // #8E9299
        accent: Color32::from_rgb(223, 160, 74),       // #DFA04A brass
        accent_soft: Color32::from_rgb(237, 190, 122), // #EDBE7A
        accent_deep: Color32::from_rgb(184, 127, 51),  // #B87F33
        success: Color32::from_rgb(79, 168, 139),      // #4FA88B verdigris
        success_hover: Color32::from_rgb(110, 199, 168),
        on_success: Color32::from_rgb(16, 26, 22),
        warning: Color32::from_rgb(242, 201, 76), // #F2C94C signal yellow
        danger: Color32::from_rgb(199, 92, 92),   // #C75C5C rust
        chrome: Color32::from_rgb(17, 20, 23),    // #111417
        scrim: Color32::from_rgb(8, 12, 18),
        overlay: Color32::from_rgba_premultiplied(3, 4, 11, 220),
        input: Color32::from_rgb(24, 28, 32),
        ambient: Color32::from_rgb(79, 168, 139), // verdigris, as the success colour
    },
    navigation: Navigation::Top,
    features: Features {
        tools: true,
        cloud: true,
        denuvo_tab: true,
        repacks: false,
        denuvo_fix: false,
        cracked_version: false,
    },
};

// `BRAND`, `logo()` and `ICON_PNG`: Drydock's, or those of the brand this build was made with.
include!(concat!(env!("OUT_DIR"), "/brand.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an sRGB colour.
    fn luminance(color: Color32) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// A palette has to stay readable, not only look right in one screenshot. This runs against
    /// Drydock's and against the brand the build was made with, so a white-label repository whose
    /// `brand.toml` picks unreadable colours fails its own tests.
    #[test]
    fn every_palette_keeps_its_text_readable() {
        for brand in [DRYDOCK, BRAND] {
            let palette = brand.palette;
            for ground in [
                palette.background,
                palette.surface,
                palette.surface_raised,
                palette.chrome,
            ] {
                assert!(contrast(palette.text, ground) >= 7.0, "{}: text", brand.name);
            }
            // Secondary text lives on the page, on panels and in the bars: full AA there.
            for ground in [palette.background, palette.surface, palette.chrome] {
                assert!(contrast(palette.muted, ground) >= 4.5, "{}: muted", brand.name);
            }
            // A raised surface is a hovered row; the muted text on it is the same text the row
            // shows at rest, so a slightly lower floor there is Drydock's long-standing choice.
            assert!(
                contrast(palette.muted, palette.surface_raised) >= 4.0,
                "{}: muted on raised",
                brand.name
            );
            assert!(
                contrast(palette.accent, palette.background) >= 3.0,
                "{}: accent",
                brand.name
            );
            assert!(
                contrast(palette.accent_soft, palette.surface) >= 4.5,
                "{}: accent_soft",
                brand.name
            );
            assert!(
                contrast(palette.on_success, palette.success) >= 4.5,
                "{}: on success",
                brand.name
            );
        }
    }

    #[test]
    fn the_brand_name_is_the_one_the_core_names_the_files_after() {
        assert_eq!(DRYDOCK.name, drydock_core::brand::DRYDOCK.name);
        assert_eq!(BRAND.name, drydock_core::PRODUCT.name);
    }

    #[test]
    fn every_product_credits_somebody() {
        for brand in [DRYDOCK, BRAND] {
            assert!(!brand.developer.trim().is_empty(), "{}: developer", brand.name);
        }
    }

    /// The Discord button opens whatever the brand names in the browser, so it has to be an https
    /// link to Discord — `build.rs` refuses anything else in a brand file, and this holds Drydock's
    /// own value to the same rule.
    #[test]
    fn a_discord_link_leads_to_discord() {
        for brand in [DRYDOCK, BRAND] {
            let Some(invite) = brand.discord else {
                continue;
            };
            let host = invite
                .strip_prefix("https://")
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default();
            assert!(
                [
                    "discord.gg",
                    "discord.com",
                    "www.discord.com",
                    "discordapp.com",
                    "www.discordapp.com"
                ]
                .contains(&host),
                "{}: {invite:?} is not an https Discord link",
                brand.name
            );
            assert!(drydock_core::is_http_url(invite));
        }
    }
}
