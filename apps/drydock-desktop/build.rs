//! Build-time setup of the desktop app: which brand it is, and the Windows resources that follow.
//!
//! A white-label build carries a `brand/` folder at the workspace root (or names one in
//! `DRYDOCK_BRAND_DIR`) holding a `brand.toml` and, optionally, its artwork — see docs/branding.md.
//! This script turns that into `$OUT_DIR/brand.rs`, which `src/brand.rs` includes. Without a brand
//! folder the build is Drydock and nothing below changes anything.

use std::fs;
use std::path::{Path, PathBuf};

/// Every key a brand file may set. Anything else is refused, so a typo fails the build instead of
/// silently falling back to Drydock's value.
const TOP_LEVEL_KEYS: &[&str] = &[
    "name",
    "tagline",
    "description",
    "logo_is_wordmark",
    "discord",
    "developer",
    "navigation",
    "features",
    "palette",
];
/// The hosts a brand's Discord link may point at: the button says "Discord", so it leads there.
const DISCORD_HOSTS: &[&str] = &[
    "discord.gg",
    "discord.com",
    "www.discord.com",
    "discordapp.com",
    "www.discordapp.com",
];
const FEATURE_KEYS: &[&str] = &[
    "tools",
    "cloud",
    "denuvo_tab",
    "repacks",
    "denuvo_fix",
    "cracked_version",
];
const PALETTE_KEYS: &[&str] = &[
    "background",
    "surface",
    "surface_raised",
    "surface_sunken",
    "border",
    "edge",
    "text",
    "muted",
    "accent",
    "accent_soft",
    "accent_deep",
    "success",
    "success_hover",
    "on_success",
    "warning",
    "danger",
    "chrome",
    "scrim",
    "overlay",
    "input",
    "ambient",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    // apps/drydock-desktop → the workspace root.
    let workspace = manifest.ancestors().nth(2).expect("workspace root").to_path_buf();
    let brand = brand_directory(&workspace);
    let spec = brand.as_deref().map(Spec::read);
    let generated = match (&brand, &spec) {
        (Some(directory), Some(spec)) => spec.generate(directory, &workspace),
        _ => drydock_generated(&workspace),
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("brand.rs");
    fs::write(&out, generated).expect("write the generated brand");

    #[cfg(windows)]
    windows_resources(&workspace, brand.as_deref(), spec.as_ref());
}

/// The brand folder, when this build has one: `DRYDOCK_BRAND_DIR` (relative paths from the
/// workspace root), else `brand/` at the workspace root. Only an existing brand file is watched —
/// watching a path that does not exist makes Cargo rebuild the crate on every build.
fn brand_directory(workspace: &Path) -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed=DRYDOCK_BRAND_DIR");
    let directory = match std::env::var("DRYDOCK_BRAND_DIR") {
        Ok(value) if !value.trim().is_empty() => workspace.join(value.trim()),
        _ => workspace.join("brand"),
    };
    let file = directory.join("brand.toml");
    if !file.is_file() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", file.display());
    for asset in ["logo.png", "app-icon.png", "app-icon.ico"] {
        let path = directory.join(asset);
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    Some(directory)
}

/// A brand file, checked.
struct Spec {
    file: PathBuf,
    name: String,
    tagline: Option<String>,
    description: Option<String>,
    logo_is_wordmark: Option<bool>,
    /// The invite link; an empty string in the file means "no button", as does leaving it out.
    discord: Option<String>,
    developer: Option<String>,
    side_navigation: Option<bool>,
    features: Vec<(String, bool)>,
    /// `(key, [r, g, b, a], has_alpha)`.
    palette: Vec<(String, [u8; 4], bool)>,
}

impl Spec {
    fn read(directory: &Path) -> Self {
        let file = directory.join("brand.toml");
        let text = fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        let value: toml::Value = text
            .parse()
            .unwrap_or_else(|error| panic!("{} is not valid TOML: {error}", file.display()));
        let table = value
            .as_table()
            .unwrap_or_else(|| panic!("{} must be a TOML table", file.display()));
        let fail = |message: String| -> ! { panic!("{}: {message}", file.display()) };
        for key in table.keys() {
            if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
                fail(format!(
                    "unknown key `{key}` (known: {})",
                    TOP_LEVEL_KEYS.join(", ")
                ));
            }
        }
        let text_of = |key: &str| {
            table.get(key).map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| fail(format!("`{key}` must be a string")))
                    .to_owned()
            })
        };
        let name = text_of("name").unwrap_or_else(|| fail("needs a `name`".into()));
        let logo_is_wordmark = table.get("logo_is_wordmark").map(|value| {
            value
                .as_bool()
                .unwrap_or_else(|| fail("`logo_is_wordmark` must be true or false".into()))
        });
        let discord = text_of("discord")
            .map(|link| link.trim().to_owned())
            .filter(|link| !link.is_empty());
        if let Some(link) = &discord
            && !is_discord_link(link)
        {
            fail(format!(
                "`discord` must be an https link to {} (e.g. \"https://discord.gg/abc123\"), not {link:?}",
                DISCORD_HOSTS.join(", ")
            ));
        }
        let developer = text_of("developer").map(|name| name.trim().to_owned());
        if let Some(name) = &developer
            && (name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control))
        {
            fail(format!(
                "`developer` must be a name of 1 to 64 characters on one line, not {name:?}"
            ));
        }
        let side_navigation = text_of("navigation").map(|value| match value.as_str() {
            "top" => false,
            "side" => true,
            other => fail(format!("`navigation` is \"top\" or \"side\", not {other:?}")),
        });

        let mut features = Vec::new();
        if let Some(section) = table.get("features") {
            let section = section
                .as_table()
                .unwrap_or_else(|| fail("[features] must be a table".into()));
            for (key, value) in section {
                if !FEATURE_KEYS.contains(&key.as_str()) {
                    fail(format!(
                        "unknown feature `{key}` (known: {})",
                        FEATURE_KEYS.join(", ")
                    ));
                }
                let on = value
                    .as_bool()
                    .unwrap_or_else(|| fail(format!("feature `{key}` must be true or false")));
                features.push((key.clone(), on));
            }
        }

        let mut palette = Vec::new();
        if let Some(section) = table.get("palette") {
            let section = section
                .as_table()
                .unwrap_or_else(|| fail("[palette] must be a table".into()));
            for (key, value) in section {
                if !PALETTE_KEYS.contains(&key.as_str()) {
                    fail(format!(
                        "unknown colour `{key}` (known: {})",
                        PALETTE_KEYS.join(", ")
                    ));
                }
                let hex = value
                    .as_str()
                    .unwrap_or_else(|| fail(format!("colour `{key}` must be a string like \"#1A2B3C\"")));
                let (rgba, has_alpha) = parse_colour(hex)
                    .unwrap_or_else(|| fail(format!("colour `{key}`: {hex:?} is not #RRGGBB or #RRGGBBAA")));
                palette.push((key.clone(), rgba, has_alpha));
            }
        }

        Self {
            name,
            tagline: text_of("tagline"),
            description: text_of("description"),
            logo_is_wordmark,
            discord,
            developer,
            side_navigation,
            features,
            palette,
            file,
        }
    }

    /// The Rust for `src/brand.rs`: the brand as a `Brand` value, anything it leaves out taken from
    /// Drydock, plus its artwork (or Drydock's where it has none).
    fn generate(&self, directory: &Path, workspace: &Path) -> String {
        let mut palette = String::new();
        for (key, [r, g, b, a], has_alpha) in &self.palette {
            // An alpha colour is written premultiplied, the only form a `const` can build.
            let colour = if *has_alpha {
                let premultiply = |channel: u8| (u16::from(channel) * u16::from(*a) / 255) as u8;
                format!(
                    "Color32::from_rgba_premultiplied({}, {}, {}, {a})",
                    premultiply(*r),
                    premultiply(*g),
                    premultiply(*b)
                )
            } else {
                format!("Color32::from_rgb({r}, {g}, {b})")
            };
            palette.push_str(&format!("        {key}: {colour},\n"));
        }
        let mut features = String::new();
        for (key, on) in &self.features {
            features.push_str(&format!("        {key}: {on},\n"));
        }
        // Whatever the brand leaves out is Drydock's; a brand that sets everything gets no
        // fallback line, which Clippy would otherwise flag as a needless struct update.
        if self.palette.len() < PALETTE_KEYS.len() {
            palette.push_str("        ..DRYDOCK.palette\n");
        }
        if self.features.len() < FEATURE_KEYS.len() {
            features.push_str("        ..DRYDOCK.features\n");
        }
        let tagline = self
            .tagline
            .as_deref()
            .map_or_else(|| "DRYDOCK.tagline".to_owned(), |text| format!("{text:?}"));
        let logo_is_wordmark = self
            .logo_is_wordmark
            .map_or_else(|| "DRYDOCK.logo_is_wordmark".to_owned(), |on| on.to_string());
        // Not Drydock's when left out: a product without a server of its own shows no button.
        let discord = self
            .discord
            .as_deref()
            .map_or_else(|| "None".to_owned(), |link| format!("Some({link:?})"));
        let developer = self
            .developer
            .as_deref()
            .map_or_else(|| "DRYDOCK.developer".to_owned(), |name| format!("{name:?}"));
        let navigation = match self.side_navigation {
            Some(true) => "Navigation::Side",
            Some(false) => "Navigation::Top",
            None => "DRYDOCK.navigation",
        };
        let asset = |name: &str, fallback: &str| {
            let own = directory.join(name);
            let path = if own.is_file() {
                own
            } else {
                workspace.join("assets").join(fallback)
            };
            format!("{:?}", path.display().to_string())
        };
        format!(
            "// Generated by build.rs from {file}. Edit that file, not this one.\n\
             pub const BRAND: Brand = Brand {{\n\
             \x20   name: drydock_core::PRODUCT.name,\n\
             \x20   tagline: {tagline},\n\
             \x20   logo_is_wordmark: {logo_is_wordmark},\n\
             \x20   discord: {discord},\n\
             \x20   developer: {developer},\n\
             \x20   palette: Palette {{\n{palette}    }},\n\
             \x20   navigation: {navigation},\n\
             \x20   features: Features {{\n{features}    }},\n\
             }};\n\
             {artwork}",
            file = self.file.display(),
            artwork = artwork(
                &asset("logo.png", "app-icon.png"),
                &asset("app-icon.png", "app-icon.png")
            ),
        )
    }
}

/// The generated brand for Drydock itself.
fn drydock_generated(workspace: &Path) -> String {
    let icon = format!(
        "{:?}",
        workspace.join("assets/app-icon.png").display().to_string()
    );
    format!("pub const BRAND: Brand = DRYDOCK;\n{}", artwork(&icon, &icon))
}

/// The logo and the window icon, compiled in from the given (already quoted) paths.
fn artwork(logo: &str, icon: &str) -> String {
    format!(
        "/// The product's logo for the navigation.\n\
         pub fn logo() -> egui::ImageSource<'static> {{\n\
         \x20   egui::ImageSource::Bytes {{\n\
         \x20       uri: std::borrow::Cow::Borrowed(\"bytes://brand/logo.png\"),\n\
         \x20       bytes: egui::load::Bytes::Static(include_bytes!({logo})),\n\
         \x20   }}\n\
         }}\n\
         /// The window icon.\n\
         pub const ICON_PNG: &[u8] = include_bytes!({icon});\n"
    )
}

/// Whether `link` is an https link to Discord, with nothing in it that has no place in a URL.
fn is_discord_link(link: &str) -> bool {
    let Some(rest) = link.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    DISCORD_HOSTS.contains(&host)
        && link
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '<' | '>'))
}

/// `#RRGGBB` or `#RRGGBBAA`, with whether the alpha was given.
fn parse_colour(text: &str) -> Option<([u8; 4], bool)> {
    let hex = text.trim().strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    let alpha = if hex.len() == 8 { channel(6)? } else { 255 };
    Some(([channel(0)?, channel(2)?, channel(4)?, alpha], hex.len() == 8))
}

/// Explorer's "Details" tab, the taskbar and the file icon: the brand's, or Drydock's.
#[cfg(windows)]
fn windows_resources(workspace: &Path, brand: Option<&Path>, spec: Option<&Spec>) {
    let (name, description) = match spec {
        Some(spec) => {
            let description = spec.description.clone().unwrap_or_else(|| match &spec.tagline {
                Some(tagline) => format!("{} — {tagline}", spec.name),
                None => spec.name.clone(),
            });
            (spec.name.clone(), description)
        }
        None => (
            "Drydock".to_owned(),
            "Drydock — Download, Activate, Play".to_owned(),
        ),
    };
    let icon = brand
        .map(|directory| directory.join("app-icon.ico"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| workspace.join("assets/app-icon.ico"));
    let mut resource = winres::WindowsResource::new();
    resource.set("ProductName", &name);
    resource.set("FileDescription", &description);
    resource.set("LegalCopyright", "Drydock contributors");
    if let Some(developer) = spec.and_then(|spec| spec.developer.as_deref()) {
        resource.set("CompanyName", developer);
    }
    resource.set_icon(&icon.display().to_string());
    if let Err(error) = resource.compile() {
        println!("cargo:warning=Windows resources could not be embedded: {error}");
    }
}
