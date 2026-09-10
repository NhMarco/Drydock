//! Local Steam-emulator (`steam_settings`) template generator — the account-free part of the
//! DiscordActivator "template generator", rebuilt to run entirely on the client.
//!
//! It writes the game-specific Cold Client Loader / gbe_fork files that don't need a logged-in
//! Steam account: `steam_appid.txt`, `coldloader.ini`, `configs.app.ini`, `depots.txt`,
//! `supported_languages.txt`, `configs.user.ini` and an (empty) `achievements.json`. Depot IDs come
//! from the depot manifest Drydock already fetches; the App ID and the exe sub-folder are supplied by
//! the caller. The account-only pieces of the upstream generator (DLC list + name from Steam PICS,
//! achievements from the Steam Web API) are intentionally left out.
//!
//! File contents match the upstream `_build_steam_template_bytes` byte-for-byte so a template made
//! here drops into the same Cold Client Loader skeleton. When the caller supplies that skeleton ZIP
//! (the shared emu DLLs / sounds / configs, identical for every game), [`EmuTemplateInput::build_files`]
//! merges it in — re-rooting its `Launcher\` entries at the exe folder and skipping the files this
//! generator regenerates — so the output is a complete, ready-to-drop-in template.

use std::io::Read;

use thiserror::Error;

/// Files this generator always (re)writes, so they are never copied from the skeleton.
const GENERATED_BASENAMES: &[&str] = &[
    "coldloader.ini",
    "steam_appid.txt",
    "configs.app.ini",
    "depots.txt",
    "supported_languages.txt",
    "achievements.json",
    "configs.user.ini",
    "configs.overlay.ini",
    "configs.main.ini",
    "controls.txt",
];

/// Static gbe_fork overlay config: keep the (experimental) overlay off by default. Matches the
/// DiscordActivator skeleton byte-for-byte (CRLF, trailing CRLF).
const CONFIGS_OVERLAY_INI: &str = "[overlay::general]\r\nenable_experimental_overlay = 0\r\n";
/// Static gbe_fork connectivity config: allow internet play, not LAN-only. Byte-for-byte skeleton.
const CONFIGS_MAIN_INI: &str = "[main::connectivity]\r\ndisable_lan_only=1\r\n";
/// Static gbe_fork controller mapping (Xbox layout). Byte-for-byte skeleton (CRLF, no trailing EOL).
const CONTROLLER_CONTROLS_TXT: &str = "AxisL=LJOY=joystick_move\r\nAxisR=RJOY=joystick_move\r\n\
AnalogL=LTRIGGER=trigger\r\nAnalogR=RTRIGGER=trigger\r\nLUp=DUP\r\nLDown=DDOWN\r\nLLeft=DLEFT\r\n\
LRight=DRIGHT\r\nRUp=Y\r\nRDown=A\r\nRLeft=X\r\nRRight=B\r\nCLeft=BACK\r\nCRight=START\r\n\
LStickPush=LSTICK\r\nRStickPush=RSTICK\r\nLTrigTop=LBUMPER\r\nRTrigTop=RBUMPER";

/// The skeleton ZIP roots every entry under this folder.
const SKELETON_LAUNCHER_PREFIX: &str = "Launcher\\";

#[derive(Debug, Error)]
pub enum EmuTemplateError {
    #[error("the skeleton archive is invalid: {0}")]
    Skeleton(String),
}

/// A Windows PE executable's target architecture, used to pick the matching emu DLLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeArch {
    X64,
    X86,
}

impl PeArch {
    /// The folder name the emu toolchain uses for this architecture (`x64` / `x86`).
    #[must_use]
    pub fn folder(self) -> &'static str {
        match self {
            PeArch::X64 => "x64",
            PeArch::X86 => "x86",
        }
    }
}

/// Reads a Windows executable's PE header and returns its architecture, or `None` when the bytes
/// aren't a recognisable PE (so the caller can ask the user which bitness to use). Mirrors
/// `cold_auto_cracker.py`'s `detect_pe_architecture`: `MZ` magic → PE offset at 0x3C → `PE\0\0` →
/// the 2-byte machine field (0x8664 = x64, 0x014C = x86).
#[must_use]
pub fn detect_pe_arch(exe: &[u8]) -> Option<PeArch> {
    if exe.len() < 0x40 || &exe[0..2] != b"MZ" {
        return None;
    }
    let pe_offset = u32::from_le_bytes([exe[0x3C], exe[0x3D], exe[0x3E], exe[0x3F]]) as usize;
    if exe.len() < pe_offset + 6 || &exe[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return None;
    }
    let machine = u16::from_le_bytes([exe[pe_offset + 4], exe[pe_offset + 5]]);
    match machine {
        0x8664 => Some(PeArch::X64),
        0x014C => Some(PeArch::X86),
        _ => None,
    }
}

/// Inputs for one template: everything the client can resolve without a Steam account.
#[derive(Clone, Debug, Default)]
pub struct EmuTemplateInput {
    pub app_id: u32,
    /// Content depot IDs (from the depot manifest).
    pub depots: Vec<u32>,
    /// DLC App IDs (from the public Steam store API).
    pub dlcs: Vec<u32>,
    /// Supported languages; defaults to `["english"]` when empty.
    pub languages: Vec<String>,
    /// The game exe's sub-folder relative to the install root (e.g. `Bin64` or
    /// `Binaries/Win64`). Empty means the exe sits at the game root.
    pub exe_dir: String,
    /// The gbe_fork `achievements.json` array as text (from the proxy's app-schema route). Empty
    /// or blank means "no achievements" and is written as `[]`.
    pub achievements_json: String,
}

impl EmuTemplateInput {
    /// The Windows-backslash folder prefix derived from `exe_dir` (e.g. `Bin64\`), or empty.
    fn prefix(&self) -> String {
        let cleaned = self.exe_dir.replace('/', "\\");
        let cleaned = cleaned.trim_matches('\\');
        if cleaned.is_empty() {
            String::new()
        } else {
            format!("{cleaned}\\")
        }
    }

    /// The generated files as `(relative Windows-backslash path, contents)` pairs, ready to write
    /// into the game folder (or a chosen output folder).
    #[must_use]
    pub fn generated_files(&self) -> Vec<(String, String)> {
        let app_id = self.app_id;
        let prefix = self.prefix();
        let ss = format!("{prefix}steam_settings\\");

        let mut languages: Vec<&str> = self
            .languages
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect();
        if languages.is_empty() {
            languages.push("english");
        }

        let depots_txt: String = self.depots.iter().map(|depot| format!("{depot}\n")).collect();
        let languages_txt: String = languages.iter().map(|lang| format!("{lang}\n")).collect();
        let app_ini: String = "[app::dlcs]\nunlock_all = 0\n".to_owned()
            + &self
                .dlcs
                .iter()
                .map(|dlc| format!("{dlc} = DLC\n"))
                .collect::<String>();
        let achievements = {
            let trimmed = self.achievements_json.trim();
            if trimmed.is_empty() { "[]" } else { trimmed }
        };

        vec![
            (
                format!("{prefix}coldloader.ini"),
                format!("[settings]\nappid = {app_id}\ncleanup_delay = 70\n"),
            ),
            (format!("{ss}steam_appid.txt"), format!("{app_id}\n")),
            (format!("{ss}configs.app.ini"), app_ini),
            (format!("{ss}depots.txt"), depots_txt),
            (format!("{ss}supported_languages.txt"), languages_txt),
            (format!("{ss}achievements.json"), achievements.to_owned()),
            (
                format!("{ss}configs.user.ini"),
                "[user::general]\naccount_name=Player\naccount_steamid=0\nticket=\nlanguage=english\n"
                    .to_owned(),
            ),
            (format!("{ss}configs.overlay.ini"), CONFIGS_OVERLAY_INI.to_owned()),
            (format!("{ss}configs.main.ini"), CONFIGS_MAIN_INI.to_owned()),
            (
                format!("{ss}controller\\controls.txt"),
                CONTROLLER_CONTROLS_TXT.to_owned(),
            ),
        ]
    }

    /// The Windows-backslash `steam_settings\` folder for this template (e.g. `Bin64\steam_settings\`),
    /// so the caller can place binary extras (achievement images, overlay sound, load_dlls stubs) it
    /// gathers itself (they need network/cache, unlike the pure config files above).
    #[must_use]
    pub fn steam_settings_dir(&self) -> String {
        format!("{}steam_settings\\", self.prefix())
    }

    /// The Windows-backslash folder the exe lives in (e.g. `Bin64\`, or empty at the game root), so
    /// the caller can drop the optional REFramework `dinput8.dll` next to the exe.
    #[must_use]
    pub fn exe_folder(&self) -> String {
        self.prefix()
    }

    /// The full set of `(relative Windows-backslash path, bytes)` files for the template: the
    /// generated config files, plus — when `skeleton_zip` is given — the shared skeleton (emu DLLs,
    /// sounds, shared configs) re-rooted at the exe folder, skipping the files this generator writes
    /// and any leftover game images. Mirrors the upstream `_build_steam_template_bytes` merge.
    pub fn build_files(
        &self,
        skeleton_zip: Option<&[u8]>,
    ) -> Result<Vec<(String, Vec<u8>)>, EmuTemplateError> {
        let prefix = self.prefix();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();

        if let Some(bytes) = skeleton_zip {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                .map_err(|error| EmuTemplateError::Skeleton(error.to_string()))?;
            for index in 0..archive.len() {
                let mut entry = archive
                    .by_index(index)
                    .map_err(|error| EmuTemplateError::Skeleton(error.to_string()))?;
                if entry.is_dir() {
                    continue;
                }
                let name = entry.name().replace('/', "\\");
                // Skeleton entries live under `Launcher\`; re-root them at the exe folder.
                let rel = name
                    .strip_prefix(SKELETON_LAUNCHER_PREFIX)
                    .unwrap_or(&name)
                    .to_owned();
                let lowered = rel.replace('\\', "/").to_lowercase();
                let basename = rel.rsplit('\\').next().unwrap_or("");
                if basename.is_empty()
                    || GENERATED_BASENAMES.contains(&basename)
                    || lowered.contains("/image/")
                {
                    continue; // regenerated below / drop any leftover game images
                }
                let mut content = Vec::with_capacity(crate::safe_path::capacity_hint(entry.size()));
                entry
                    .read_to_end(&mut content)
                    .map_err(|error| EmuTemplateError::Skeleton(error.to_string()))?;
                files.push((format!("{prefix}{rel}"), content));
            }
        }

        for (path, content) in self.generated_files() {
            files.push((path, content.into_bytes()));
        }
        Ok(files)
    }
}

/// Extracts the distinct achievement-icon image references from a gbe_fork `achievements.json`
/// array, as `(file name, url)` pairs. Only `http(s)` `icon`/`icongray` values are returned (the
/// account-free proxy schema always emits full Steam CDN URLs). The file name is the URL's last path
/// segment — the same name the icon keeps in `steam_settings\image\`, so the overlay finds it
/// offline. Duplicate URLs (icon shared between achievements) are de-duplicated.
#[must_use]
pub fn achievement_image_urls(achievements_json: &str) -> Vec<(String, String)> {
    let trimmed = achievements_json.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut images: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        for key in ["icon", "icongray"] {
            let Some(url) = entry.get(key).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let url = url.trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            // Strip any query/fragment, then take the last path segment as the file name.
            let path = url.split(['?', '#']).next().unwrap_or(url);
            let name = path.rsplit('/').next().unwrap_or("").trim();
            if name.is_empty() || !seen.insert(url.to_owned()) {
                continue;
            }
            images.push((name.to_owned(), url.to_owned()));
        }
    }
    images
}

/// Packs `(Windows-backslash path, bytes)` files into a ZIP (entries use forward slashes, so the
/// game's folder structure is preserved). Used to save a crack as a ZIP instead of deploying it.
pub fn zip_files(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, EmuTemplateError> {
    use std::io::Write;
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buffer);
    let options: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (relative, contents) in files {
        let name = relative.replace('\\', "/");
        zip.start_file(name, options)
            .map_err(|error| EmuTemplateError::Skeleton(error.to_string()))?;
        zip.write_all(contents)
            .map_err(|error| EmuTemplateError::Skeleton(error.to_string()))?;
    }
    zip.finish()
        .map_err(|error| EmuTemplateError::Skeleton(error.to_string()))?;
    Ok(buffer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_expected_files_with_exe_prefix() {
        let input = EmuTemplateInput {
            app_id: 730,
            depots: vec![731, 732],
            dlcs: vec![900, 901],
            languages: vec![],
            exe_dir: "Binaries/Win64".to_owned(),
            achievements_json: String::new(),
        };
        let files: std::collections::HashMap<String, String> = input.generated_files().into_iter().collect();

        assert_eq!(
            files["Binaries\\Win64\\coldloader.ini"],
            "[settings]\nappid = 730\ncleanup_delay = 70\n"
        );
        assert_eq!(files["Binaries\\Win64\\steam_settings\\steam_appid.txt"], "730\n");
        assert_eq!(files["Binaries\\Win64\\steam_settings\\depots.txt"], "731\n732\n");
        assert_eq!(
            files["Binaries\\Win64\\steam_settings\\configs.app.ini"],
            "[app::dlcs]\nunlock_all = 0\n900 = DLC\n901 = DLC\n"
        );
        // Empty languages default to english.
        assert_eq!(
            files["Binaries\\Win64\\steam_settings\\supported_languages.txt"],
            "english\n"
        );
        assert_eq!(files["Binaries\\Win64\\steam_settings\\achievements.json"], "[]");
        // Static gbe_fork configs + controller mapping are always emitted, byte-for-byte.
        assert_eq!(
            files["Binaries\\Win64\\steam_settings\\configs.overlay.ini"],
            "[overlay::general]\r\nenable_experimental_overlay = 0\r\n"
        );
        assert_eq!(
            files["Binaries\\Win64\\steam_settings\\configs.main.ini"],
            "[main::connectivity]\r\ndisable_lan_only=1\r\n"
        );
        assert!(
            files["Binaries\\Win64\\steam_settings\\controller\\controls.txt"].starts_with("AxisL=LJOY=")
        );
    }

    #[test]
    fn achievement_image_urls_dedup_and_names() {
        let json = r#"[
            {"name":"A","icon":"https://cdn/apps/1/aaa.jpg","icongray":"https://cdn/apps/1/bbb.jpg"},
            {"name":"B","icon":"https://cdn/apps/1/aaa.jpg","icongray":"local_only.jpg"},
            {"name":"C","icon":"https://cdn/apps/1/ccc.jpg?t=9"}
        ]"#;
        let images = achievement_image_urls(json);
        // aaa is de-duplicated; local_only (no scheme) is dropped; query is stripped from the name.
        assert_eq!(
            images,
            vec![
                ("aaa.jpg".to_owned(), "https://cdn/apps/1/aaa.jpg".to_owned()),
                ("bbb.jpg".to_owned(), "https://cdn/apps/1/bbb.jpg".to_owned()),
                ("ccc.jpg".to_owned(), "https://cdn/apps/1/ccc.jpg?t=9".to_owned()),
            ]
        );
        assert!(achievement_image_urls("").is_empty());
        assert!(achievement_image_urls("[]").is_empty());
    }

    #[test]
    fn empty_exe_dir_puts_files_at_root() {
        let input = EmuTemplateInput {
            app_id: 480,
            depots: vec![],
            dlcs: vec![],
            languages: vec!["english".into(), "german".into()],
            exe_dir: String::new(),
            achievements_json: "[{\"name\":\"ACH\"}]".to_owned(),
        };
        let files: std::collections::HashMap<String, String> = input.generated_files().into_iter().collect();
        assert!(files.contains_key("coldloader.ini"));
        assert_eq!(files["steam_settings\\steam_appid.txt"], "480\n");
        assert_eq!(files["steam_settings\\depots.txt"], "");
        assert_eq!(
            files["steam_settings\\supported_languages.txt"],
            "english\ngerman\n"
        );
        assert_eq!(files["steam_settings\\achievements.json"], "[{\"name\":\"ACH\"}]");
    }

    #[test]
    fn detect_pe_arch_reads_machine_field() {
        // Minimal PE stub: MZ, PE offset 0x40 at 0x3C, `PE\0\0` + machine at 0x40.
        let mut x64 = vec![0u8; 0x48];
        x64[0] = b'M';
        x64[1] = b'Z';
        x64[0x3C] = 0x40;
        x64[0x40..0x44].copy_from_slice(b"PE\0\0");
        x64[0x44] = 0x64;
        x64[0x45] = 0x86; // 0x8664 little-endian
        assert_eq!(detect_pe_arch(&x64), Some(PeArch::X64));

        let mut x86 = x64.clone();
        x86[0x44] = 0x4C;
        x86[0x45] = 0x01; // 0x014C
        assert_eq!(detect_pe_arch(&x86), Some(PeArch::X86));

        assert_eq!(detect_pe_arch(b"not a pe"), None);
    }

    #[test]
    fn build_files_merges_skeleton_and_skips_generated_and_images() {
        // A tiny skeleton ZIP: a shared DLL, a shared config, one file the generator regenerates,
        // and a game image — all under `Launcher\`.
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, content) in [
                ("Launcher\\steamclient64.dll", &b"DLL"[..]),
                ("Launcher\\steam_settings\\steam_interfaces.txt", &b"IFACE"[..]), // shared → copied
                ("Launcher\\steam_settings\\depots.txt", &b"SKELETON"[..]),        // regenerated → skipped
                ("Launcher\\steam_settings\\image/logo.jpg", &b"IMG"[..]),         // image → skipped
            ] {
                writer.start_file(name, options).unwrap();
                std::io::Write::write_all(&mut writer, content).unwrap();
            }
            writer.finish().unwrap();
        }

        let input = EmuTemplateInput {
            app_id: 730,
            depots: vec![10],
            dlcs: vec![],
            languages: vec![],
            exe_dir: "Bin64".to_owned(),
            achievements_json: String::new(),
        };
        let files: std::collections::HashMap<String, Vec<u8>> =
            input.build_files(Some(&buffer)).unwrap().into_iter().collect();

        // Skeleton DLL + shared config re-rooted at the exe folder.
        assert_eq!(files["Bin64\\steamclient64.dll"], b"DLL");
        assert_eq!(files["Bin64\\steam_settings\\steam_interfaces.txt"], b"IFACE");
        // depots.txt comes from the generator, not the skeleton.
        assert_eq!(files["Bin64\\steam_settings\\depots.txt"], b"10\n");
        // The game image was dropped.
        assert!(!files.keys().any(|path| path.contains("image")));
    }
}
