//! Fetches the Cold Client Loader emu toolchain (the shared DLLs) and caches it in the Drydock data
//! dir, so a generated template can ship the real emu binaries — no Steam account, no manual
//! skeleton. **Both** architectures are downloaded once; the caller then picks the set matching the
//! game's exe.
//!
//!   * the public Drydock repo's `emu/` folder → `version.dll` (the loader proxy; deployed as
//!     version.dll or winmm.dll), `coldloader.dll`, and the SteamStub loader that goes into
//!     `steam_settings\load_dlls\`
//!   * gbe_fork's Windows release archive (from its latest GitHub release) → the experimental
//!     `steamclient(64).dll` + `GameOverlayRenderer(64).dll` + `steam_api(64).dll`
//!
//! Hosting the first group ourselves means a DLL can be swapped by pushing to the repo, without a
//! Drydock release and without depending on a third party's release assets staying put.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::emu_template::PeArch;
use crate::version::user_agent;

/// The `emu/` folder of the public Drydock repository, which hosts every emulator DLL except the
/// gbe_fork ones. Served raw over HTTPS: the repository is public, so no token and no proxy detour.
const EMU_BASE: &str = "https://raw.githubusercontent.com/NhMarco/Drydock/main/emu";

/// What to fetch from [`EMU_BASE`], as `(arch subfolder, file name in the repo, cached file name)`.
/// The repo distinguishes architectures by suffix; the cache uses one subfolder per architecture, so
/// the deployed names stay the plain ones Cold Client Loader expects.
const REPO_DLLS: &[(&str, &str, &str)] = &[
    ("x64", "version_x64.dll", "version.dll"),
    ("x64", "coldloader_x64.dll", "coldloader.dll"),
    ("x64", "steamstub_x64.dll", "steamstub_x64.dll"),
    ("x86", "version_x32.dll", "version.dll"),
    ("x86", "coldloader_x32.dll", "coldloader.dll"),
    ("x86", "steamstub_x32.dll", "steamstub_x32.dll"),
];

/// gbe_fork's GitHub repository; the Windows build comes from its latest release.
const GBE_REPOSITORY: &str = "Detanup01/gbe_fork";
/// The names gbe_fork has published its Windows release build under, preferred first. The release
/// of 2026-09-16 dropped `emu-win-release.7z` and ships only the Visual Studio 2022 build (just as
/// statically linked, and laid out the same), so one fixed download URL is not enough.
const GBE_ARCHIVES: &[&str] = &["emu-win-release.7z", "emu-win-release-vs22.7z"];
/// Where the gbe_fork archive is cached, whatever the release called it.
const GBE_CACHE_NAME: &str = "emu-win-release.7z";
/// Release metadata is a few kilobytes; anything far larger is not what was asked for.
const MAXIMUM_RELEASE_JSON_BYTES: u64 = 1024 * 1024;
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
// experimental `steam_api`; v4: loader/coldloader now come from the Drydock repo, plus steamstub).
const READY_MARKER: &str = ".ready4";

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

/// The DLLs the cache must hold beyond [`REPO_DLLS`], as `(arch subfolder, file name)` — these are
/// the ones extracted from the gbe_fork archive.
const CACHED_DLLS: &[(&str, &str)] = &[
    ("x64", "steamclient64.dll"),
    ("x64", "GameOverlayRenderer64.dll"),
    ("x64", "steam_api64.dll"),
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
        && REPO_DLLS
            .iter()
            .all(|(arch, _, cached)| root.join(arch).join(cached).is_file())
        && root.join("shared").join(OVERLAY_SOUND_NAME).is_file()
}

/// The cached gbe_fork overlay achievement sound bytes, or `None` when it isn't cached (e.g. the gbe
/// release dropped it, or antivirus removed it) — the sound is a nicety, so callers just skip it.
#[must_use]
pub fn overlay_sound_bytes(data_root: &Path) -> Option<Vec<u8>> {
    fs::read(data_root.join("emu").join("shared").join(OVERLAY_SOUND_NAME)).ok()
}

/// The cached SteamStub loader for `arch` as `(relative path under steam_settings, bytes)`. gbe_fork
/// loads whatever it finds in `load_dlls`, so exactly one loader goes in there. Returns nothing when
/// the file is missing from the cache (e.g. antivirus removed it) — the caller still produces a
/// usable crack, just without the SteamStub step.
#[must_use]
pub fn load_dll_files(data_root: &Path, arch: PeArch) -> Vec<(String, Vec<u8>)> {
    let name = steamstub_name(arch);
    match fs::read(data_root.join("emu").join(arch.folder()).join(name)) {
        Ok(bytes) => vec![(format!("load_dlls\\{name}"), bytes)],
        Err(_) => Vec::new(),
    }
}

/// The SteamStub loader's file name for `arch`, identical in the repo and in the cache.
#[must_use]
pub fn steamstub_name(arch: PeArch) -> &'static str {
    match arch {
        PeArch::X64 => "steamstub_x64.dll",
        PeArch::X86 => "steamstub_x32.dll",
    }
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

    // The loader proxy, coldloader.dll and the SteamStub loader come straight from the Drydock repo
    // as plain files — no archive to unpack, so a replaced DLL there reaches users on the next fetch.
    for (arch, repo_name, cached_name) in REPO_DLLS {
        let bytes = download(&client, &format!("{EMU_BASE}/{repo_name}"))?;
        write_file(&root.join(arch).join(cached_name), &bytes)?;
    }

    // gbe_fork ships all steamclient DLLs in one 7z; cache the archive, then extract the ones we need.
    let cache = archives.join(GBE_CACHE_NAME);
    let gbe_urls = || gbe_archive_urls(&client);
    let (gbe_bytes, cached) = get_archive(&client, gbe_urls, is_seven_z, &cache, force)?;
    if let Err(error) = extract_gbe_steamclient(&gbe_bytes, &root) {
        // A cached archive that no longer extracts (cut short, say) is fetched once more, rather
        // than failing every crack from now on. A blocked or unwritable DLL is not the archive's
        // fault, and a fresh download would fail the same way.
        if !cached || matches!(error, EmuToolchainError::Blocked | EmuToolchainError::Io { .. }) {
            return Err(error);
        }
        let (fresh, _) = get_archive(&client, gbe_urls, is_seven_z, &cache, true)?;
        extract_gbe_steamclient(&fresh, &root)?;
    }

    write_file(&root.join(READY_MARKER), b"ok")?;
    Ok(root)
}

/// Returns an archive's bytes and whether they came from the cache: from `cache_path` when it holds
/// a valid archive (and not `force`), otherwise downloaded from the first of `urls` that serves one
/// and written to `cache_path`, so later runs reuse it instead of re-downloading.
fn get_archive(
    client: &reqwest::blocking::Client,
    urls: impl FnOnce() -> Vec<String>,
    valid: fn(&[u8]) -> bool,
    cache_path: &Path,
    force: bool,
) -> Result<(Vec<u8>, bool), EmuToolchainError> {
    if !force
        && let Ok(bytes) = fs::read(cache_path)
        && valid(&bytes)
    {
        return Ok((bytes, true));
    }
    let bytes = download_first(client, &urls(), valid)?;
    write_file(cache_path, &bytes)?;
    Ok((bytes, false))
}

/// Downloads the first of `urls` that serves a `valid` archive. The error is the last one met.
fn download_first(
    client: &reqwest::blocking::Client,
    urls: &[String],
    valid: fn(&[u8]) -> bool,
) -> Result<Vec<u8>, EmuToolchainError> {
    let mut last_error = None;
    for url in urls {
        match download(client, url) {
            Ok(bytes) if valid(&bytes) => return Ok(bytes),
            Ok(_) => {
                last_error = Some(EmuToolchainError::Archive(format!(
                    "{url} did not serve the expected archive"
                )));
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| EmuToolchainError::Download {
        url: "no download address".to_owned(),
    }))
}

fn is_seven_z(bytes: &[u8]) -> bool {
    bytes.starts_with(b"7z\xBC\xAF\x27\x1C")
}

fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// Where to download gbe_fork's Windows release archive from, best first: the asset its latest
/// release actually lists, then every known name under `releases/latest/download/`, for when the
/// GitHub API is unreachable or rate-limited.
fn gbe_archive_urls(client: &reqwest::blocking::Client) -> Vec<String> {
    let mut urls: Vec<String> = latest_gbe_asset(client).into_iter().collect();
    urls.extend(
        GBE_ARCHIVES
            .iter()
            .map(|name| format!("https://github.com/{GBE_REPOSITORY}/releases/latest/download/{name}")),
    );
    urls
}

fn latest_gbe_asset(client: &reqwest::blocking::Client) -> Option<String> {
    let response = client
        .get(format!(
            "https://api.github.com/repos/{GBE_REPOSITORY}/releases/latest"
        ))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .ok()?;
    if response
        .content_length()
        .is_some_and(|size| size > MAXIMUM_RELEASE_JSON_BYTES)
    {
        return None;
    }
    let release: GitHubRelease = response.json().ok()?;
    pick_gbe_asset(&release.assets)
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// The download URL of the Windows release build among a release's assets: a known name in order of
/// preference, else any other `emu-win-release*.7z` — never a debug build or another platform's.
fn pick_gbe_asset(assets: &[GitHubAsset]) -> Option<String> {
    GBE_ARCHIVES
        .iter()
        .find_map(|name| assets.iter().find(|asset| asset.name.eq_ignore_ascii_case(name)))
        .or_else(|| {
            assets.iter().find(|asset| {
                let name = asset.name.to_ascii_lowercase();
                name.starts_with("emu-win-release") && name.ends_with(".7z")
            })
        })
        .filter(|asset| asset.browser_download_url.starts_with("https://github.com/"))
        .map(|asset| asset.browser_download_url.clone())
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

    let mut reader =
        sevenz_rust2::ArchiveReader::new(Cursor::new(seven_z_bytes), sevenz_rust2::Password::empty())
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
    let (zip, _) = get_archive(
        &client,
        || vec![REFRAMEWORK.to_owned()],
        is_zip,
        &archives.join("REFramework.zip"),
        force,
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_7z_preserves_both_architectures_and_optional_sound() {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        for sound in [false, true] {
            let root = tempfile::tempdir().unwrap();
            for subdir in ["x64", "x86", "shared"] {
                fs::create_dir(root.path().join(subdir)).unwrap();
            }
            let mut names = vec![
                "ignored.txt",
                "steamclient_experimental/steamclient64.dll",
                "steamclient_experimental/gameoverlayrenderer64.dll",
                "steamclient_experimental/steamclient.dll",
                "steamclient_experimental/gameoverlayrenderer.dll",
                "experimental/x64/steam_api64.dll",
                "experimental/x86/steam_api.dll",
            ];
            if sound {
                names.push(OVERLAY_SOUND_NAME);
            }
            let entries = names.iter().map(|name| ArchiveEntry::new_file(name)).collect();
            let readers = names
                .iter()
                .map(|name| SourceReader::new(std::io::Cursor::new(name.as_bytes())))
                .collect();
            let mut writer = ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
            writer.push_archive_entries(entries, readers).unwrap();
            let archive = writer.finish().unwrap().into_inner();
            extract_gbe_steamclient(&archive, root.path()).unwrap();
            assert!(root.path().join("x64/steam_api64.dll").is_file());
            assert!(root.path().join("x86/steamclient.dll").is_file());
            assert!(!root.path().join("ignored.txt").exists());
            assert_eq!(
                root.path().join("shared").join(OVERLAY_SOUND_NAME).exists(),
                sound
            );
            assert!(extract_gbe_steamclient(&archive[..archive.len() / 2], root.path()).is_err());
        }
    }

    fn asset(name: &str) -> GitHubAsset {
        GitHubAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://github.com/x/releases/download/tag/{name}"),
        }
    }

    #[test]
    fn the_windows_release_build_is_found_whatever_the_release_calls_it() {
        let picked = |names: &[&str]| {
            let assets: Vec<GitHubAsset> = names.iter().map(|name| asset(name)).collect();
            pick_gbe_asset(&assets).map(|url| url.rsplit('/').next().unwrap_or_default().to_owned())
        };
        // Both builds published: the one Drydock always used.
        assert_eq!(
            picked(&[
                "emu-win-debug.7z",
                "emu-win-release-vs22.7z",
                "emu-win-release.7z"
            ])
            .as_deref(),
            Some("emu-win-release.7z")
        );
        // The release of 2026-09-16 has only the Visual Studio build.
        assert_eq!(
            picked(&[
                "emu-linux-release.tar.bz2",
                "emu-win-debug-vs22.7z",
                "emu-win-release-vs22.7z",
                "migrate_gse-win.7z"
            ])
            .as_deref(),
            Some("emu-win-release-vs22.7z")
        );
        // A build variant nobody has seen yet still counts; debug and other platforms never do.
        assert_eq!(
            picked(&["emu-win-release-clang.7z"]).as_deref(),
            Some("emu-win-release-clang.7z")
        );
        assert_eq!(
            picked(&["emu-win-debug-vs22.7z", "emu-linux-release.tar.bz2"]),
            None
        );

        let elsewhere = GitHubAsset {
            name: "emu-win-release.7z".to_owned(),
            browser_download_url: "https://example.com/emu-win-release.7z".to_owned(),
        };
        assert_eq!(pick_gbe_asset(&[elsewhere]), None);
    }

    #[test]
    fn archives_are_recognised_by_their_signature() {
        assert!(is_seven_z(b"7z\xBC\xAF\x27\x1C\x00\x04"));
        assert!(!is_seven_z(b"<!DOCTYPE html>"));
        assert!(!is_seven_z(b""));
        assert!(is_zip(b"PK\x03\x04rest"));
        assert!(!is_zip(b"Not Found"));
    }

    #[test]
    fn a_cached_archive_is_used_only_while_it_is_valid() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("archive.7z");
        let client = http_client().unwrap();
        let no_urls = || -> Vec<String> { Vec::new() };

        fs::write(&cache, b"7z\xBC\xAF\x27\x1C cached").unwrap();
        let (bytes, cached) = get_archive(&client, no_urls, is_seven_z, &cache, false).unwrap();
        assert!(cached);
        assert!(bytes.ends_with(b"cached"));

        // An HTML error page cached by an older build is not an archive, so it is fetched again.
        fs::write(&cache, b"<html>Not Found</html>").unwrap();
        assert!(get_archive(&client, no_urls, is_seven_z, &cache, false).is_err());
    }

    #[test]
    #[ignore = "downloads the gbe_fork release from GitHub"]
    fn the_latest_gbe_release_downloads_and_extracts() {
        let client = http_client().unwrap();
        let urls = gbe_archive_urls(&client);
        assert!(
            urls.len() > GBE_ARCHIVES.len(),
            "the GitHub API lists a Windows build"
        );
        let bytes = download_first(&client, &urls, is_seven_z).unwrap();
        let root = tempfile::tempdir().unwrap();
        for subdir in ["x64", "x86", "shared"] {
            fs::create_dir(root.path().join(subdir)).unwrap();
        }
        match extract_gbe_steamclient(&bytes, root.path()) {
            // Windows Security quarantines emulator DLLs unless the folder is excluded.
            Ok(()) | Err(EmuToolchainError::Blocked) => {}
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn load_dlls_gets_exactly_the_loader_for_the_architecture() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (arch, expected) in [
            (PeArch::X64, r"load_dlls\steamstub_x64.dll"),
            (PeArch::X86, r"load_dlls\steamstub_x32.dll"),
        ] {
            let dir = temp.path().join("emu").join(arch.folder());
            fs::create_dir_all(&dir).expect("cache dir");
            fs::write(dir.join(steamstub_name(arch)), b"stub").expect("cache file");

            let files = load_dll_files(temp.path(), arch);
            assert_eq!(files.len(), 1, "exactly one loader belongs in load_dlls");
            assert_eq!(files[0].0, expected);
            assert_eq!(files[0].1, b"stub");
        }
    }

    #[test]
    fn a_missing_cached_loader_is_skipped_rather_than_failing() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(load_dll_files(temp.path(), PeArch::X64).is_empty());
    }

    #[test]
    fn every_repo_dll_lands_in_an_architecture_folder() {
        for (arch, repo_name, cached) in REPO_DLLS {
            assert!(*arch == "x64" || *arch == "x86", "unexpected arch folder {arch}");
            let suffix = if *arch == "x64" { "_x64.dll" } else { "_x32.dll" };
            assert!(
                repo_name.ends_with(suffix),
                "{repo_name} does not carry the {arch} suffix, so it would be cached under the wrong \
                 architecture"
            );
            assert!(cached.ends_with(".dll"));
        }
    }
}
