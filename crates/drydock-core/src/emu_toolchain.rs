//! Fetches the Cold Client Loader emu toolchain (the shared DLLs) straight from the public GitHub
//! releases and caches it in the Drydock data dir, so a generated template can ship the real emu
//! binaries — no Steam account, no manual skeleton. **Both** architectures are downloaded once; the
//! caller then picks the set matching the game's exe. Mirrors the download list in
//! `cold_auto_cracker.py`:
//!
//!   * `coldloader-proxy-{arch}.zip`  → `version.dll` (the loader proxy; deployed as version.dll or
//!     winmm.dll)
//!   * `coldloader-release-{arch}.zip`→ `coldloader.dll`
//!   * `emu-win-release.7z` (gbe_fork)→ the experimental `steamclient(64).dll` +
//!     `GameOverlayRenderer(64).dll` + `steam_api(64).dll`

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::emu_template::PeArch;
use crate::version::user_agent;

const PROXY_X64: &str =
    "https://github.com/denuvosanctuary/coldloader-proxy/releases/latest/download/coldloader-proxy-x64.zip";
const PROXY_X86: &str =
    "https://github.com/denuvosanctuary/coldloader-proxy/releases/latest/download/coldloader-proxy-x86.zip";
const LOADER_X64: &str =
    "https://github.com/denuvosanctuary/coldloader/releases/latest/download/coldloader-release-x64.zip";
const LOADER_X86: &str =
    "https://github.com/denuvosanctuary/coldloader/releases/latest/download/coldloader-release-x86.zip";
const GBE: &str = "https://github.com/Detanup01/gbe_fork/releases/latest/download/emu-win-release.7z";
/// praydog's REFramework nightly — the latest `REFramework.zip`, which contains `dinput8.dll`. Used
/// only when the user opts a game in (some Denuvo/RE-Engine titles need it next to the exe).
const REFRAMEWORK: &str =
    "https://github.com/praydog/reframework-nightly/releases/latest/download/REFramework.zip";

/// The gbe_fork overlay achievement sound, cached shared (arch-independent) and deployed into
/// `steam_settings\sounds\`.
const OVERLAY_SOUND_NAME: &str = "overlay_achievement_notification.wav";

/// Written once the toolchain has been fully extracted, so a later run skips the download.
// Bumped when the set/variant of extracted DLLs changes, so an existing cache re-extracts instead of
// keeping stale files (v2: big `steamclient_experimental` steamclient + overlay; v3: added the gbe
// experimental `steam_api`).
const READY_MARKER: &str = ".ready3";

#[derive(Debug, Error)]
pub enum EmuToolchainError {
    #[error("download failed for {url}")]
    Download { url: String },
    #[error("archive error: {0}")]
    Archive(String),
    #[error(
        "your antivirus blocked the emulator files (Steam emulators are always flagged). Add a \
         Windows Security exclusion for the Drydock cache folder and the game folder, then retry."
    )]
    Blocked,
    #[error("the emu release did not contain {0}")]
    Missing(String),
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// One deployable DLL: the file name to write and the cached source path to copy from.
pub struct ToolchainDll {
    pub deploy_name: String,
    pub source: PathBuf,
}

/// The DLLs the cache must hold, as `(arch subfolder, file name)`.
const CACHED_DLLS: &[(&str, &str)] = &[
    ("x64", "version.dll"),
    ("x64", "coldloader.dll"),
    ("x64", "steamclient64.dll"),
    ("x64", "GameOverlayRenderer64.dll"),
    ("x64", "steam_api64.dll"),
    ("x86", "version.dll"),
    ("x86", "coldloader.dll"),
    ("x86", "steamclient.dll"),
    ("x86", "GameOverlayRenderer.dll"),
    ("x86", "steam_api.dll"),
];

/// Whether every cached emu DLL is present on disk (so an antivirus that removed one triggers a
/// refetch rather than a confusing missing-file error later).
#[must_use]
pub fn toolchain_ready(data_root: &Path) -> bool {
    let root = data_root.join("emu");
    root.join(READY_MARKER).is_file()
        && CACHED_DLLS
            .iter()
            .all(|(arch, dll)| root.join(arch).join(dll).is_file())
        && root.join("shared").join(OVERLAY_SOUND_NAME).is_file()
}

/// The cached gbe_fork overlay achievement sound bytes, or `None` when it isn't cached (e.g. the gbe
/// release dropped it, or antivirus removed it) — the sound is a nicety, so callers just skip it.
#[must_use]
pub fn overlay_sound_bytes(data_root: &Path) -> Option<Vec<u8>> {
    fs::read(data_root.join("emu").join("shared").join(OVERLAY_SOUND_NAME)).ok()
}

/// The embedded generic x64 `load_dlls` stub DLLs as `(relative path under steam_settings, bytes)`,
/// for x64 deploys. Re-exported from [`crate::emu_load_dlls`] so the cracker adds them alongside the
/// toolchain. x86 games get none (the stubs are x64-only).
#[must_use]
pub fn load_dll_files(arch: PeArch) -> Vec<(String, Vec<u8>)> {
    if arch != PeArch::X64 {
        return Vec::new();
    }
    crate::emu_load_dlls::load_dll_stubs()
        .into_iter()
        .map(|(name, bytes)| (format!("load_dlls\\{name}"), bytes))
        .collect()
}

/// Ensures both architectures of the emu toolchain are cached under `data_root/emu/`. Downloads and
/// extracts on the first run, when `force`, or when a cached DLL has gone missing (e.g. deleted by
/// antivirus); otherwise returns the cached folder immediately so it is reused, not re-downloaded.
pub fn ensure_toolchain(data_root: &Path, force: bool) -> Result<PathBuf, EmuToolchainError> {
    let root = data_root.join("emu");
    if !force && toolchain_ready(data_root) {
        return Ok(root);
    }
    // Keep any already-cached archives so re-extracting (e.g. after antivirus removed a DLL) doesn't
    // re-download; just make sure the folders exist.
    create_dir(&root)?;
    create_dir(&root.join("x64"))?;
    create_dir(&root.join("x86"))?;
    create_dir(&root.join("shared"))?;
    let archives = root.join("archives");
    create_dir(&archives)?;

    let client = http_client()?;

    // The loader proxy (version.dll) and the coldloader.dll, per architecture, from their ZIPs.
    for (arch, proxy_url, loader_url) in [("x64", PROXY_X64, LOADER_X64), ("x86", PROXY_X86, LOADER_X86)] {
        let proxy_zip = get_archive(
            &client,
            proxy_url,
            &archives.join(format!("coldloader-proxy-{arch}.zip")),
            force,
        )?;
        let version_dll = zip_file_by_name(&proxy_zip, "version.dll")?;
        write_file(&root.join(arch).join("version.dll"), &version_dll)?;

        let loader_zip = get_archive(
            &client,
            loader_url,
            &archives.join(format!("coldloader-release-{arch}.zip")),
            force,
        )?;
        let coldloader_dll = zip_file_by_name(&loader_zip, "coldloader.dll")?;
        write_file(&root.join(arch).join("coldloader.dll"), &coldloader_dll)?;
    }

    // gbe_fork ships all steamclient DLLs in one 7z; cache the archive, then extract the four we need.
    let gbe_bytes = get_archive(&client, GBE, &archives.join("emu-win-release.7z"), force)?;
    extract_gbe_steamclient(&gbe_bytes, &root)?;

    write_file(&root.join(READY_MARKER), b"ok")?;
    Ok(root)
}

/// Returns an archive's bytes: from the on-disk cache when present (and not `force`), otherwise
/// downloaded and written to `cache_path` so later runs reuse it instead of re-downloading.
fn get_archive(
    client: &reqwest::blocking::Client,
    url: &str,
    cache_path: &Path,
    force: bool,
) -> Result<Vec<u8>, EmuToolchainError> {
    if !force
        && let Ok(bytes) = fs::read(cache_path)
        && !bytes.is_empty()
    {
        return Ok(bytes);
    }
    let bytes = download(client, url)?;
    write_file(cache_path, &bytes)?;
    Ok(bytes)
}

/// The DLLs to deploy for `arch`, in the order Cold Client Loader expects. `loader_name` is the
/// proxy DLL's deployed name (`version.dll` or `winmm.dll`, depending on the game).
#[must_use]
pub fn toolchain_dlls(root: &Path, arch: PeArch, loader_name: &str) -> Vec<ToolchainDll> {
    let dir = root.join(arch.folder());
    let (steamclient, overlay, steam_api) = match arch {
        PeArch::X64 => (
            "steamclient64.dll",
            "GameOverlayRenderer64.dll",
            "steam_api64.dll",
        ),
        PeArch::X86 => ("steamclient.dll", "GameOverlayRenderer.dll", "steam_api.dll"),
    };
    vec![
        ToolchainDll {
            deploy_name: loader_name.to_owned(),
            source: dir.join("version.dll"),
        },
        ToolchainDll {
            deploy_name: "coldloader.dll".to_owned(),
            source: dir.join("coldloader.dll"),
        },
        ToolchainDll {
            deploy_name: steamclient.to_owned(),
            source: dir.join(steamclient),
        },
        ToolchainDll {
            deploy_name: overlay.to_owned(),
            source: dir.join(overlay),
        },
        // The gbe emulated Steamworks API, replacing the game's own steam_api next to the exe.
        ToolchainDll {
            deploy_name: steam_api.to_owned(),
            source: dir.join(steam_api),
        },
    ]
}

fn http_client() -> Result<reqwest::blocking::Client, EmuToolchainError> {
    reqwest::blocking::Client::builder()
        .user_agent(user_agent())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| EmuToolchainError::Download {
            url: "client".to_owned(),
        })
}

fn download(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, EmuToolchainError> {
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|_| EmuToolchainError::Download { url: url.to_owned() })?;
    response
        .bytes()
        .map(|bytes| bytes.to_vec())
        .map_err(|_| EmuToolchainError::Download { url: url.to_owned() })
}

/// Finds the first entry whose base name equals `name` (case-insensitive) and returns its bytes.
fn zip_file_by_name(zip_bytes: &[u8], name: &str) -> Result<Vec<u8>, EmuToolchainError> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|error| EmuToolchainError::Archive(error.to_string()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| EmuToolchainError::Archive(error.to_string()))?;
        let entry_name = entry.name().replace('\\', "/");
        let basename = entry_name.rsplit('/').next().unwrap_or("");
        if basename.eq_ignore_ascii_case(name) {
            let mut bytes = Vec::with_capacity(crate::safe_path::capacity_hint(entry.size()));
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| EmuToolchainError::Archive(error.to_string()))?;
            return Ok(bytes);
        }
    }
    Err(EmuToolchainError::Missing(name.to_owned()))
}

/// Pulls the four steamclient DLLs (and the overlay achievement sound) out of the gbe_fork 7z **in
/// memory** (the bytes come from the on-disk archive cache or a fresh download), writing only those
/// to `root/<arch>/` (DLLs) and `root/shared/` (sound) — no other files are extracted. Antivirus may
/// block the emu DLLs (or the cached archive) on write; that surfaces as [`EmuToolchainError::Blocked`].
/// The sound is optional: if the release ever drops it, extraction still succeeds.
fn extract_gbe_steamclient(seven_z_bytes: &[u8], root: &Path) -> Result<(), EmuToolchainError> {
    use std::io::{Cursor, Write};

    // Path suffix (lowercased, forward slashes) -> destination path under `root`. We take the big
    // `steamclient_experimental` DLLs (the full standalone steamclient + overlay, ~20 MB each) — the
    // same variant the reference cracker deploys — not the small `experimental/<arch>` steamclient
    // that shares the base name. DLLs go to their arch folder; the overlay sound is shared.
    let wanted: &[(&str, PathBuf)] = &[
        (
            "steamclient_experimental/steamclient64.dll",
            root.join("x64").join("steamclient64.dll"),
        ),
        (
            "steamclient_experimental/gameoverlayrenderer64.dll",
            root.join("x64").join("gameoverlayrenderer64.dll"),
        ),
        (
            "steamclient_experimental/steamclient.dll",
            root.join("x86").join("steamclient.dll"),
        ),
        (
            "steamclient_experimental/gameoverlayrenderer.dll",
            root.join("x86").join("gameoverlayrenderer.dll"),
        ),
        // The gbe emulated Steamworks API, deployed next to the game exe. The `experimental` variant
        // is the one paired with the `steamclient_experimental` steamclient above.
        (
            "experimental/x64/steam_api64.dll",
            root.join("x64").join("steam_api64.dll"),
        ),
        (
            "experimental/x86/steam_api.dll",
            root.join("x86").join("steam_api.dll"),
        ),
        (OVERLAY_SOUND_NAME, root.join("shared").join(OVERLAY_SOUND_NAME)),
    ];
    // Every wanted file except the (optional) sound must be extracted for the toolchain to be ready.
    let required = wanted.len() - 1;

    let mut reader = sevenz_rust::SevenZReader::new(
        Cursor::new(seven_z_bytes),
        seven_z_bytes.len() as u64,
        sevenz_rust::Password::empty(),
    )
    .map_err(|error| EmuToolchainError::Archive(error.to_string()))?;

    let mut written_required = 0usize;
    let mut blocked = false;
    let mut hard_error: Option<std::io::Error> = None;
    reader
        .for_each_entries(|entry, entry_reader| {
            if entry.is_directory() {
                return Ok(true);
            }
            let path = entry.name().replace('\\', "/").to_lowercase();
            let target = wanted.iter().find(|(suffix, _)| path.ends_with(suffix));
            // A 7z is often a single solid stream, so every entry must be read fully and in order or
            // the checksum fails. Non-wanted entries are read into a throwaway sink.
            let Some((keep_name, dest)) = target else {
                std::io::copy(entry_reader, &mut std::io::sink())?;
                return Ok(true);
            };
            let mut bytes = Vec::new();
            std::io::copy(entry_reader, &mut bytes)?;
            let is_sound = *keep_name == OVERLAY_SOUND_NAME;
            match fs::File::create(dest).and_then(|mut file| file.write_all(&bytes)) {
                Ok(()) if !is_sound => written_required += 1,
                Ok(()) => {}
                Err(error) if is_av_block(&error) => blocked = true,
                Err(error) => {
                    hard_error = Some(error);
                    return Ok(false); // stop iteration
                }
            }
            Ok(true)
        })
        .map_err(|error| {
            if is_av_block_str(&error.to_string()) {
                EmuToolchainError::Blocked
            } else {
                EmuToolchainError::Archive(error.to_string())
            }
        })?;

    if let Some(error) = hard_error {
        return Err(EmuToolchainError::Io {
            path: root.to_path_buf(),
            source: error,
        });
    }
    if blocked {
        return Err(EmuToolchainError::Blocked);
    }
    if written_required < required {
        return Err(EmuToolchainError::Missing("steamclient DLLs".to_owned()));
    }
    Ok(())
}

/// Best-effort download of the achievement icon images (from [`crate::achievement_image_urls`]),
/// returning `(file name, bytes)` for the ones that fetched. Failures are skipped silently — the
/// icons are a convenience (gbe_fork also loads them from the CDN at runtime), so a missing image
/// never fails the crack. One shared keep-alive client is reused across the (often 100+) requests.
#[must_use]
pub fn fetch_achievement_images(urls: &[(String, String)]) -> Vec<(String, Vec<u8>)> {
    if urls.is_empty() {
        return Vec::new();
    }
    let Ok(client) = http_client() else {
        return Vec::new();
    };
    urls.iter()
        .filter_map(|(name, url)| download(&client, url).ok().map(|bytes| (name.clone(), bytes)))
        .collect()
}

/// Downloads praydog's latest REFramework nightly and returns its `dinput8.dll` bytes, caching the
/// `REFramework.zip` under `data_root/emu/archives/` so opting more games in doesn't re-download it.
/// Optional: only called when the user ticks "include REFramework" for a game.
pub fn fetch_reframework_dll(data_root: &Path, force: bool) -> Result<Vec<u8>, EmuToolchainError> {
    let archives = data_root.join("emu").join("archives");
    create_dir(&archives)?;
    let client = http_client()?;
    let zip = get_archive(&client, REFRAMEWORK, &archives.join("REFramework.zip"), force)?;
    zip_file_by_name(&zip, "dinput8.dll")
}

/// Windows raises error 225 (ERROR_VIRUS_INFECTED) when antivirus blocks a file — the emu DLLs are
/// routinely flagged, so we detect it to give a clear "add an exclusion" message.
fn is_av_block(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(225)
}

fn is_av_block_str(message: &str) -> bool {
    message.contains("225") || message.contains("Virus") || message.contains("virus")
}

fn create_dir(path: &Path) -> Result<(), EmuToolchainError> {
    fs::create_dir_all(path).map_err(|source| EmuToolchainError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), EmuToolchainError> {
    fs::write(path, bytes).map_err(|source| {
        if is_av_block(&source) {
            EmuToolchainError::Blocked
        } else {
            EmuToolchainError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })
}
