//! Fetches the "Steam Service" (OST) DLLs from the public **BetterSteamTools** GitHub releases
//! (`madoiscool/BetterSteamTools`) instead of the private MFB repo. The latest release ships an
//! `OpenSteamTool-<ver>-Release.zip` whose three DLLs — `dwmapi.dll`, `xinput1_4.dll` and
//! `OpenSteamTool.dll` — are copied into the Steam root (per the project's README), together with an
//! `opensteamtool.toml` that points OST's Lua loader at Drydock' `config/stplug-in` folder. This is the
//! account-free, public equivalent of the CloudRedirect downloader in [`crate::cloud`].
//!
//! The zip is downloaded and unpacked **in memory** (nothing is written to disk here, so antivirus
//! can't quarantine an intermediate file); the resulting [`SteamServicePackage`] is handed to the
//! unchanged [`crate::steam_service::install_service`], and a matching [`SteamServiceManifest`]
//! (with per-file Git-blob SHAs) drives [`crate::steam_service::service_status`]. A small
//! process-wide cache keyed by release tag means a status refresh only re-downloads the ~1 MB zip
//! when the upstream version actually changes.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use thiserror::Error;

use crate::mfb::{RepositoryFile, SteamServiceManifest, SteamServicePackage, compute_git_blob_sha};
use crate::version::user_agent;

const OST_REPOSITORY: &str = "madoiscool/BetterSteamTools";
/// Guard against a pathologically large release asset.
const MAX_ZIP_BYTES: u64 = 64 * 1024 * 1024;

/// OpenSteamTool's config file, written into the Steam root beside the DLLs. It points OST's Lua
/// loader at `config/stplug-in` — the folder Drydock installs per-app unlock Lua into (`add_app_files`)
/// — instead of OST's default `config/lua`, so games added through Drydock are picked up. Shipped as a
/// managed payload file so it is installed, SHA-verified and removed alongside the DLLs.
const OPENSTEAMTOOL_TOML_NAME: &str = "opensteamtool.toml";
const OPENSTEAMTOOL_TOML: &str = "[lua]\npaths = [\"config/stplug-in\"]\n";

/// Caches the last successfully unpacked release by tag, so a status refresh that finds the same
/// upstream version reuses the DLL bytes instead of downloading the zip again.
static CACHE: Mutex<Option<SteamServicePackage>> = Mutex::new(None);

/// Downloads the OpenSteamTool (OST) DLLs from the public BetterSteamTools releases.
pub struct OpenSteamTool {
    client: Client,
    repository: String,
}

impl OpenSteamTool {
    pub fn new() -> Result<Self, OstError> {
        let client = Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self {
            client,
            repository: OST_REPOSITORY.to_owned(),
        })
    }

    /// The latest published release tag (e.g. `v1.0.1`).
    pub fn latest_version(&self) -> Result<String, OstError> {
        Ok(self.latest_release()?.tag_name)
    }

    /// The Steam Service package (version + the OST DLLs) from the latest release, for installing.
    pub fn download_package(&self) -> Result<SteamServicePackage, OstError> {
        let release = self.latest_release()?;
        self.package_for(&release)
    }

    /// A manifest (version + per-file Git-blob SHAs) describing the latest release, for the status
    /// check. Built from the same (cached) package, so it and [`Self::download_package`] agree.
    pub fn manifest(&self) -> Result<SteamServiceManifest, OstError> {
        let package = self.download_package()?;
        let files = package
            .files
            .iter()
            .map(|(name, bytes)| RepositoryFile {
                relative_path: name.clone(),
                source_url: String::new(),
                sha: compute_git_blob_sha(bytes),
            })
            .collect();
        Ok(SteamServiceManifest {
            version: package.version,
            files,
        })
    }

    /// Returns the cached package when it matches the release's tag, otherwise downloads and unpacks
    /// the release's `*-Release.zip` and caches it.
    fn package_for(&self, release: &GitHubRelease) -> Result<SteamServicePackage, OstError> {
        if let Ok(guard) = CACHE.lock()
            && let Some(cached) = guard.as_ref()
            && cached.version == release.tag_name
        {
            return Ok(cached.clone());
        }

        let asset = release.release_zip_url().ok_or(OstError::AssetMissing)?;
        let zip_bytes = self.get_bytes(&asset)?;
        let mut files = extract_dlls(&zip_bytes)?;
        if files.is_empty() {
            return Err(OstError::NoDlls);
        }
        // Ship OST's config alongside the DLLs so its Lua loader reads Drydock' `config/stplug-in`.
        files.insert(
            OPENSTEAMTOOL_TOML_NAME.to_owned(),
            OPENSTEAMTOOL_TOML.as_bytes().to_vec(),
        );
        let package = SteamServicePackage {
            version: release.tag_name.clone(),
            files,
        };
        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some(package.clone());
        }
        Ok(package)
    }

    fn latest_release(&self) -> Result<GitHubRelease, OstError> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repository);
        let response = self
            .client
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()?
            .error_for_status()?;
        Ok(response.json()?)
    }

    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, OstError> {
        let mut response = self.client.get(url).send()?.error_for_status()?;
        if response.content_length().is_some_and(|size| size > MAX_ZIP_BYTES) {
            return Err(OstError::TooLarge);
        }
        let mut buffer = Vec::new();
        response
            .by_ref()
            .take(MAX_ZIP_BYTES + 1)
            .read_to_end(&mut buffer)?;
        if buffer.len() as u64 > MAX_ZIP_BYTES {
            return Err(OstError::TooLarge);
        }
        Ok(buffer)
    }
}

/// Extracts the OST DLLs (`*.dll`) from the release zip, skipping the `.exp`/`.lib` build artifacts.
/// Returns `(file name, bytes)` keyed by base file name — the shape [`SteamServicePackage`] wants.
fn extract_dlls(zip_bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, OstError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|error| OstError::Archive(error.to_string()))?;
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| OstError::Archive(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let basename = name.rsplit('/').next().unwrap_or("").to_owned();
        if !basename.to_ascii_lowercase().ends_with(".dll") {
            continue; // skip .exp / .lib build artifacts
        }
        let mut bytes = Vec::with_capacity(crate::safe_path::capacity_hint(entry.size()));
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| OstError::Archive(error.to_string()))?;
        files.insert(basename, bytes);
    }
    Ok(files)
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

impl GitHubRelease {
    /// The download URL of the `*-Release.zip` asset (the Release build, not the Debug one).
    fn release_zip_url(&self) -> Option<String> {
        self.assets
            .iter()
            .find(|asset| {
                let name = asset.name.to_ascii_lowercase();
                name.ends_with(".zip") && name.contains("release") && !name.contains("debug")
            })
            .map(|asset| asset.browser_download_url.clone())
    }
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Error)]
pub enum OstError {
    #[error("the BetterSteamTools release has no Release zip asset")]
    AssetMissing,
    #[error("the BetterSteamTools release zip contained no DLLs")]
    NoDlls,
    #[error("the BetterSteamTools release asset is too large")]
    TooLarge,
    #[error("could not read the release archive: {0}")]
    Archive(String),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_zip_url_prefers_release_over_debug() {
        let release = GitHubRelease {
            tag_name: "v1.0.1".to_owned(),
            assets: vec![
                GitHubAsset {
                    name: "OpenSteamTool-v1.0.1-Debug.zip".to_owned(),
                    browser_download_url: "https://x/debug.zip".to_owned(),
                },
                GitHubAsset {
                    name: "OpenSteamTool-v1.0.1-Release.zip".to_owned(),
                    browser_download_url: "https://x/release.zip".to_owned(),
                },
            ],
        };
        assert_eq!(
            release.release_zip_url().as_deref(),
            Some("https://x/release.zip")
        );
    }

    #[test]
    fn extract_dlls_keeps_only_dlls() {
        // A tiny in-memory zip with a DLL and a .lib build artifact.
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, content) in [
                ("OpenSteamTool.dll", &b"MZ\x00dll"[..]),
                ("OpenSteamTool.lib", &b"lib"[..]),
            ] {
                writer.start_file(name, options).unwrap();
                std::io::Write::write_all(&mut writer, content).unwrap();
            }
            writer.finish().unwrap();
        }
        let files = extract_dlls(&buffer).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files.get("OpenSteamTool.dll").map(Vec::as_slice),
            Some(&b"MZ\x00dll"[..])
        );
    }
}
