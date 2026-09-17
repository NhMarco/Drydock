use egui::Color32;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};
use drydock_core::*;

use crate::ui::types::UbisoftPrepared;

/// Steam's own runtimes/redistributables that show up as installed "apps".
pub fn is_real_game(app_id: u32, name: &str) -> bool {
    // Steamworks Common Redistributables, Steam Linux Runtime(s), various Proton builds.
    pub const TOOL_APP_IDS: &[u32] = &[228_980, 1_070_560, 1_391_110, 1_628_350];
    if TOOL_APP_IDS.contains(&app_id) {
        return false;
    }
    let lower = name.to_lowercase();
    !(lower.contains("redistributable")
        || lower.contains("steam linux runtime")
        || lower.starts_with("proton"))
}

pub fn arch_from_depot_paths(data: &DepotData) -> Option<PeArch> {
    for manifest in &data.manifests {
        for file in &manifest.files {
            let path = file.path.to_ascii_lowercase();
            if !path.ends_with(".exe") && !path.ends_with(".dll") {
                continue;
            }
            if path.contains("win64") || path.contains("bin64") || path.contains("x64") {
                return Some(PeArch::X64);
            }
            if path.contains("win32") || path.contains("bin32") || path.contains("x86") {
                return Some(PeArch::X86);
            }
        }
    }
    None
}

/// Builds a Cold Client Loader crack for `app_id`: resolves the account-free config (depots from the
/// manifest, DLCs + languages from the store, achievements from the proxy) and the architecture (from
/// `arch_choice`, else Steam app-info, else the game exe's PE header), ensures the emu toolchain is
/// cached, then deploys into the game folder or writes a ZIP. Returns a summary or an error message.

pub fn human_bps(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1.0 {
        return "0 B/s".to_owned();
    }
    pub const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut value = bytes_per_sec;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Formats a count with thousands comma separators (e.g. 70616 -> "70,616").
#[allow(dead_code)]
pub fn format_number_with_commas(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    let len = s.len();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

/// A compact Downloads-panel stat: an accent-coloured caption over its value.

pub fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let start = count - max;
    let mut out = String::from("…");
    out.extend(text.chars().skip(start));
    out
}

/// Truncates `text` to at most `max` characters, appending an ellipsis when it was cut (respecting
/// char boundaries). Used to keep status-bar messages from overrunning the bar.
pub fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[allow(dead_code)]
pub fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && (bytes.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

/// The blocking Ubisoft prepare sequence (runs on a background thread): resolve the game exe via
/// Steam, download + install the magicfiles beside it, launch it once, capture the generated
/// token_req.txt, and mint the machine/App-bound activation code.

pub fn prepare_ubisoft(app_id: u32, chosen: &Path, settings_directory: &Path) -> Result<UbisoftPrepared, String> {
    if !chosen.is_dir() {
        return Err("Select the game's folder first.".to_owned());
    }
    let executables = fetch_windows_executables(app_id).map_err(|error| error.to_string())?;
    let Some(exe_relative) = executables.first() else {
        return Err("Steam lists no launch executable for this game to verify against.".to_owned());
    };
    let root = resolve_game_root(chosen, &executables).ok_or_else(|| {
        "These files don't look like the selected game. Pick the correct game folder.".to_owned()
    })?;
    let exe = root.join(exe_relative);
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "Could not resolve the game executable's folder.".to_owned())?
        .to_path_buf();

    let magic = ProxyClient::new()
        .and_then(|client| client.magicfiles(app_id))
        .map_err(|error| error.to_string())?;
    install_magicfiles(&magic, &exe_dir).map_err(|error| error.to_string())?;
    clear_previous_token_files(&exe_dir);
    let token_request =
        run_and_capture_token_request(&exe, Duration::from_secs(180)).map_err(|error| error.to_string())?;
    let activation_code = ActivationRequestService::new(settings_directory)
        .and_then(|service| service.generate_ubisoft_delivery_code(app_id, &token_request))
        .map_err(|error| error.to_string())?;
    Ok(UbisoftPrepared {
        activation_code,
        exe_dir,
    })
}

/// One segment of the Activation page's STEAM/UBISOFT/EA switcher. Returns true when clicked.

pub fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

/// A small rounded status chip with a translucent tint of `color`.
/// A small rounded chip. `warning` prepends a drawn triangle. Both variants share the exact
/// same structure so status and activation pills always render at the same height.

pub fn joined_or_unknown(values: &[String]) -> String {
    if values.is_empty() {
        "Not available".into()
    } else {
        values.join(", ")
    }
}

pub fn value_or_unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "Not available".into()
    } else {
        value.to_owned()
    }
}


pub fn is_short_activation_code(value: &str) -> bool {
    value.len() == 8 && value.chars().all(|character| character.is_ascii_alphanumeric())
}

pub fn normalize_response_fragment(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

pub fn distribute_response_code(characters: &mut [String; 8], start: usize, value: &str) -> usize {
    let normalized = normalize_response_fragment(value);
    let mut next = start.min(characters.len() - 1);
    for (offset, character) in normalized.chars().enumerate() {
        let index = start + offset;
        if index >= characters.len() {
            break;
        }
        characters[index] = character.to_string();
        next = (index + 1).min(characters.len() - 1);
    }
    next
}

/// Paints a remote image into `rect`. Returns true if the image failed to load, so callers
/// can try to resolve a better URL.


/// The Steam artwork URLs to try for an app, in order of preference. Steam has no single image that
/// exists for every app — some (unreleased titles, soundtracks, some DLC) are missing the classic
/// `header.jpg` but do have a capsule or a portrait library image — so the caller falls through the
/// list until one loads. Every entry is a real, per-app CDN path when it exists.
pub fn steam_artwork_urls(app_id: u32) -> [String; 5] {
    let base = "https://cdn.cloudflare.steamstatic.com/steam/apps";
    [
        format!("{base}/{app_id}/header.jpg"),
        // Newer / unreleased titles often have only the wide hero art on the CDN (header.jpg and the
        // capsules 404), so try it before the capsules — otherwise the library shows a blank tile.
        format!("{base}/{app_id}/library_hero.jpg"),
        format!("{base}/{app_id}/capsule_616x353.jpg"),
        format!("{base}/{app_id}/capsule_231x87.jpg"),
        format!("{base}/{app_id}/library_600x900.jpg"),
    ]
}

/// Loads the active-Denuvo App IDs: a fresh cache when available, otherwise a live curator fetch
/// (persisted for next time), falling back to any stale cache if the network is unavailable.
pub fn fetch_denuvo_appids(cache_path: &Path, force: bool) -> Result<Vec<u32>, String> {
    pub const DENUVO_CACHE_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
    if !force && let Some(appids) = load_cached_denuvo_appids(cache_path, DENUVO_CACHE_LIFETIME) {
        return Ok(appids);
    }
    match DenuvoWatchClient::new().and_then(|client| client.fetch_active_appids()) {
        Ok(appids) => {
            let _ = save_denuvo_appids(cache_path, &appids);
            Ok(appids)
        }
        Err(error) => read_denuvo_appids(cache_path).ok_or_else(|| error.to_string()),
    }
}

pub fn human_bytes(bytes: u64) -> String {
    pub const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn path_if_present(value: &str) -> Option<&Path> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| Path::new(trimmed))
}

pub fn refresh_marker_is_current(path: &Path, expected: &str, maximum_age: Duration) -> bool {
    fs::read_to_string(path).is_ok_and(|value| value == expected)
        && fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age <= maximum_age)
}

pub fn github_access_mode() -> &'static str {
    if std::env::var("DRYDOCK_GITHUB_TOKEN").is_ok_and(|token| !token.trim().is_empty()) {
        "authenticated"
    } else {
        "anonymous"
    }
}

/// Fetches the app's depot package and copies its manifests into `<steam>/depotcache`.
///
/// Runs after the unlock Lua is in place, on the same background thread. Deliberately **best
/// effort**: the Lua alone is what unlocks the app, so a depot package that is slow, unavailable or
/// still being built upstream must not undo an otherwise successful add. The outcome is folded into
/// the status note instead.
///
/// Note that this is the slow half of the operation — the upstream packages a depot on demand, which
/// takes seconds for a small title and minutes for a large one.
/// Also writes the payload to Drydock's own store, so a later install can restore it without the
/// network — see [`drydock_core::app_payloads`]. Steam deletes an app's manifests when it is
/// uninstalled, so this local copy is the only one that survives.
pub fn copy_depot_manifests_to_cache(
    proxy: &ProxyClient,
    steam_root: &Path,
    store: &AppPayloadStore,
    app_id: u32,
    lua: Option<(&str, &[u8])>,
) -> Result<usize, String> {
    let data = DepotData::fetch(proxy, app_id).map_err(|error| error.to_string())?;
    let installed =
        install_depot_manifests(steam_root, &data.raw_manifests).map_err(|error| error.to_string())?;
    // Keeping our own copy is the point of the exercise, but failing to would not undo a successful
    // add — the next install just falls back to fetching.
    let _ = store.save(app_id, lua, &data.raw_manifests);
    Ok(installed)
}

/// Puts a stored payload back where Steam expects it, before asking Steam to install.
///
/// Steam clears an app's manifests out of `depotcache` on uninstall, and re-adds nothing on
/// install — so without this a reinstall would have to pull the depot package again. Returns
/// `(lua restored, manifests restored)`.
pub fn restore_payload_into_steam(
    store: &AppPayloadStore,
    steam_root: &Path,
    app_id: u32,
) -> Result<(bool, usize), String> {
    let payload = store.load(app_id);
    if payload.is_empty() {
        return Ok((false, 0));
    }
    let mut lua_restored = false;
    if let Some((name, bytes)) = &payload.lua {
        let mut files = std::collections::BTreeMap::new();
        files.insert(name.clone(), bytes.clone());
        add_app_files(steam_root, &files).map_err(|error| error.to_string())?;
        lua_restored = true;
    }
    let manifests =
        install_depot_manifests(steam_root, &payload.manifests).map_err(|error| error.to_string())?;
    Ok((lua_restored, manifests))
}

/// Renders the note shown after an add/update, folding in how the depot-manifest copy went.
pub fn added_note(name: &str, manifests: &Result<usize, String>) -> String {
    match manifests {
        Ok(0) => format!("\"{name}\" added to Steam. No depot manifests were packaged for it."),
        Ok(count) => format!("\"{name}\" added to Steam, with {count} depot manifest(s) cached."),
        Err(_) => format!("\"{name}\" added to Steam. Depot manifests could not be cached."),
    }
}

