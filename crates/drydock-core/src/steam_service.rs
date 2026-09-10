//! Steam Service install/update/repair and per-app unlock installation.
//!
//! Rust port of the C# `SteamServiceManager`. The Steam Service payload files are
//! written directly beside `steam.exe` (into the Steam root directory), while the
//! per-app Lua unlock files are written into `<steam>/config/stplug-in`. Every write goes through
//! [`install_files_transactionally`], which stages content to a temporary directory,
//! verifies each staged and copied file by SHA-256, backs up any file it replaces, and
//! rolls the whole batch back if any step fails. Nothing is left half-installed.
//!
//! Networking lives in [`crate::proxy`]; this module operates on data that the caller has
//! already downloaded and verified, which keeps the transactional logic fully testable.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::mfb::{
    OBSOLETE_STEAM_SERVICE_FILE_NAMES, RepositoryFile, SteamServiceManifest, SteamServicePackage,
    matches_git_blob_sha,
};
use crate::steam::is_valid_steam_directory;

const MARKER_NAME: &str = ".drydock-service.json";
const LEGACY_MARKER_NAME: &str = ".closedsteamloader-service.json";
const PLUGIN_SUBDIR: [&str; 2] = ["config", "stplug-in"];

/// The install state of the Steam Service, mirrored from the C# enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteamServiceState {
    NotInstalled,
    Current,
    UpdateAvailable,
    Error,
}

/// A user-facing status with the label of the primary action button.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamServiceStatus {
    pub state: SteamServiceState,
    pub message: String,
}

impl SteamServiceStatus {
    #[must_use]
    pub fn action_text(&self) -> &'static str {
        match self.state {
            SteamServiceState::NotInstalled => "Install",
            SteamServiceState::UpdateAvailable => "Update",
            SteamServiceState::Error => "Repair",
            SteamServiceState::Current => "Reinstall",
        }
    }

    fn new(state: SteamServiceState, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct Marker {
    #[serde(default)]
    version: String,
    #[serde(default)]
    installed_utc: String,
    #[serde(default)]
    files: Vec<String>,
}

fn plugin_directory(steam_directory: &Path) -> PathBuf {
    let mut path = steam_directory.to_path_buf();
    for part in PLUGIN_SUBDIR {
        path.push(part);
    }
    path
}

/// Directory the Steam Service payload (`steamclient.dll`, `hid.dll`, `version.json`, …)
/// is installed into: directly beside `steam.exe`, i.e. the Steam root directory itself.
fn service_directory(steam_directory: &Path) -> PathBuf {
    steam_directory.to_path_buf()
}

/// Removes a Steam Service payload that an older release installed into the legacy
/// `config/stplug-in` folder, now that the payload lives beside `steam.exe`.
///
/// Only files recorded in a legacy service marker, the file names this package still ships,
/// and the known obsolete names are deleted — plus the markers themselves. Per-app Lua unlock
/// files legitimately live in `config/stplug-in` and are never touched (belt-and-suspenders:
/// `.lua` names are skipped outright). Entirely best-effort; failures are ignored because the
/// new install beside `steam.exe` has already succeeded by the time this runs.
fn remove_legacy_service_install(steam_directory: &Path, current_files: &BTreeMap<String, Vec<u8>>) {
    let legacy = plugin_directory(steam_directory);
    if !legacy.is_dir() {
        return;
    }
    let mut names: Vec<String> = current_files.keys().cloned().collect();
    for marker_name in [MARKER_NAME, LEGACY_MARKER_NAME] {
        if let Ok(text) = fs::read_to_string(legacy.join(marker_name))
            && let Ok(marker) = serde_json::from_str::<Marker>(&text)
        {
            names.extend(marker.files);
        }
    }
    names.extend(
        OBSOLETE_STEAM_SERVICE_FILE_NAMES
            .iter()
            .map(|name| (*name).to_owned()),
    );
    for name in dedup_ignore_case(&names) {
        let safe = file_name_of(&name);
        // Never delete per-app Lua unlocks — those belong in this folder.
        if safe.is_empty() || safe.to_ascii_lowercase().ends_with(".lua") {
            continue;
        }
        let _ = fs::remove_file(legacy.join(safe));
    }
    let _ = fs::remove_file(legacy.join(MARKER_NAME));
    let _ = fs::remove_file(legacy.join(LEGACY_MARKER_NAME));
}

/// Determines whether the installed service is missing, outdated, current, or broken.
///
/// The `manifest` is the remote description (version and per-file blob SHAs) that the
/// caller fetched through [`crate::proxy::ProxyClient::steam_service_manifest`].
#[must_use]
pub fn service_status(steam_directory: &Path, manifest: &SteamServiceManifest) -> SteamServiceStatus {
    if !is_valid_steam_directory(Some(steam_directory)) {
        return SteamServiceStatus::new(SteamServiceState::Error, "Select a valid Steam folder first.");
    }

    let files_current = manifest.files.iter().all(|file| {
        let path = service_directory(steam_directory).join(file.file_name());
        fs::read(&path)
            .map(|content| matches_git_blob_sha(&content, &file.sha))
            .unwrap_or(false)
    });

    let installed = read_installed_version(steam_directory);
    if installed.trim().is_empty() {
        return SteamServiceStatus::new(
            SteamServiceState::NotInstalled,
            "Steam Service is ready to install.",
        );
    }
    if !installed.eq_ignore_ascii_case(&manifest.version) {
        return SteamServiceStatus::new(
            SteamServiceState::UpdateAvailable,
            format!("Update {installed} → {} available.", manifest.version),
        );
    }
    if !files_current {
        return SteamServiceStatus::new(
            SteamServiceState::Error,
            "Steam Service files are missing, changed, or incomplete.",
        );
    }
    SteamServiceStatus::new(SteamServiceState::Current, "Steam Service is current.")
}

/// Installs or updates the Steam Service from a downloaded, verified package.
pub fn install_service(
    steam_directory: &Path,
    package: &SteamServicePackage,
) -> Result<SteamServiceStatus, ServiceError> {
    ensure_steam_directory(steam_directory)?;
    for (name, content) in &package.files {
        if content.is_empty() {
            return Err(ServiceError::EmptyFile(name.clone()));
        }
        if name.to_ascii_lowercase().ends_with(".dll")
            && (content.len() < 2 || content[0] != b'M' || content[1] != b'Z')
        {
            return Err(ServiceError::InvalidDll(name.clone()));
        }
    }

    let target_directory = service_directory(steam_directory);
    fs::create_dir_all(&target_directory)?;
    let marker_path = target_directory.join(MARKER_NAME);

    // Files that a previous install or an older release left behind and that this
    // package no longer ships must be removed as part of the same transaction.
    let mut obsolete: Vec<String> = read_installed_file_names(steam_directory);
    obsolete.extend(
        OBSOLETE_STEAM_SERVICE_FILE_NAMES
            .iter()
            .map(|name| (*name).to_owned()),
    );
    obsolete.retain(|name| !package.files.contains_key(name));

    let previous_marker = fs::read(&marker_path).ok();

    let mut ordered_names: Vec<String> = package.files.keys().cloned().collect();
    ordered_names.sort_by_key(|name| name.to_ascii_lowercase());
    let marker = Marker {
        version: package.version.clone(),
        installed_utc: jiff::Timestamp::now().to_string(),
        files: ordered_names,
    };
    let marker_json = serde_json::to_vec_pretty(&marker)?;

    let commit = {
        let marker_path = marker_path.clone();
        move || -> Result<(), ServiceError> {
            let temporary = marker_path.with_extension("json.new");
            write_atomically(&temporary, &marker_path, &marker_json)?;
            Ok(())
        }
    };

    let result = install_files_transactionally(&target_directory, &package.files, &obsolete, Some(commit));
    if result.is_err() {
        // Restore the marker to its previous content so status reporting stays truthful.
        match &previous_marker {
            Some(bytes) => {
                let _ = fs::write(&marker_path, bytes);
            }
            None => {
                let _ = fs::remove_file(&marker_path);
            }
        }
        result?;
    }

    let _ = fs::remove_file(target_directory.join(LEGACY_MARKER_NAME));
    // Clean up an earlier install that put the payload into `config/stplug-in`.
    remove_legacy_service_install(steam_directory, &package.files);
    Ok(SteamServiceStatus::new(
        SteamServiceState::Current,
        "Steam Service installed successfully.",
    ))
}

/// Removes the installed Steam Service files and marker, with backup and rollback.
///
/// Uses the recorded marker to know which files to delete and also clears the known
/// obsolete file names. Returns a `NotInstalled` status on success.
pub fn uninstall_service(steam_directory: &Path) -> Result<SteamServiceStatus, ServiceError> {
    ensure_steam_directory(steam_directory)?;
    let plugin = service_directory(steam_directory);
    if !plugin.is_dir() {
        return Ok(SteamServiceStatus::new(
            SteamServiceState::NotInstalled,
            "Steam Service is not installed.",
        ));
    }

    let mut names: Vec<String> = read_installed_file_names(steam_directory);
    names.extend(
        OBSOLETE_STEAM_SERVICE_FILE_NAMES
            .iter()
            .map(|name| (*name).to_owned()),
    );
    let names = dedup_ignore_case(&names);

    let transaction = TempTransaction::new()?;
    let backup = transaction.subdir("backup")?;
    let mut removed: Vec<String> = Vec::new();
    let outcome = (|| -> Result<(), ServiceError> {
        for name in &names {
            let safe = file_name_of(name);
            if safe.is_empty() || safe != name {
                return Err(ServiceError::InvalidObsoleteName);
            }
            let destination = plugin.join(safe);
            if !destination.is_file() {
                continue;
            }
            fs::copy(&destination, backup.join(safe))?;
            fs::remove_file(&destination)?;
            removed.push(safe.to_owned());
            if destination.exists() {
                return Err(ServiceError::PostDeleteVerification(safe.to_owned()));
            }
        }
        Ok(())
    })();

    if outcome.is_err() {
        roll_back(&plugin, &backup, removed.iter());
        outcome?;
    }

    // The markers are best-effort: their absence already means "not installed".
    let _ = fs::remove_file(plugin.join(MARKER_NAME));
    let _ = fs::remove_file(plugin.join(LEGACY_MARKER_NAME));
    // Also clear any leftovers from an old `config/stplug-in` install.
    remove_legacy_service_install(steam_directory, &BTreeMap::new());
    Ok(SteamServiceStatus::new(
        SteamServiceState::NotInstalled,
        "Steam Service removed.",
    ))
}

/// Installs the Lua unlock files for one app, transactionally.
///
/// `payload` maps each Lua file name to its downloaded, blob-SHA-verified content.
/// Returns the number of files written.
pub fn add_app_files(
    steam_directory: &Path,
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<usize, ServiceError> {
    ensure_steam_directory(steam_directory)?;
    if payload.is_empty() {
        return Err(ServiceError::NoLuaFiles);
    }
    for name in payload.keys() {
        let safe = file_name_of(name);
        if safe.is_empty() || safe != name {
            return Err(ServiceError::InvalidLuaName);
        }
    }
    let target = create_plugin_directory(steam_directory)?;
    install_files_transactionally(&target, payload, &[], None::<fn() -> Result<(), ServiceError>>)?;
    Ok(payload.len())
}

/// Removes the given Lua unlock file names for an app, with backup and rollback.
///
/// Returns the number of files actually removed.
pub fn remove_app_files(steam_directory: &Path, lua_names: &[String]) -> Result<usize, ServiceError> {
    ensure_steam_directory(steam_directory)?;
    let mut names: Vec<String> = Vec::new();
    for name in lua_names {
        let safe = file_name_of(name);
        if !safe.is_empty()
            && safe.to_ascii_lowercase().ends_with(".lua")
            && !names.iter().any(|existing| existing.eq_ignore_ascii_case(safe))
        {
            names.push(safe.to_owned());
        }
    }
    if names.is_empty() {
        return Err(ServiceError::NoLuaFiles);
    }

    let target = plugin_directory(steam_directory);
    if !target.is_dir() {
        return Ok(0);
    }

    let transaction = TempTransaction::new()?;
    let backup = transaction.subdir("backup")?;
    let mut removed: Vec<String> = Vec::new();
    let outcome = (|| -> Result<usize, ServiceError> {
        for name in &names {
            let destination = target.join(name);
            if !destination.is_file() {
                continue;
            }
            fs::copy(&destination, backup.join(name))?;
            fs::remove_file(&destination)?;
            removed.push(name.clone());
            if destination.exists() {
                return Err(ServiceError::PostDeleteVerification(name.clone()));
            }
        }
        Ok(removed.len())
    })();

    if outcome.is_err() {
        // Restore anything already removed before the failure.
        for name in removed.iter().rev() {
            let source = backup.join(name);
            if source.is_file() {
                let _ = fs::copy(&source, target.join(name));
            }
        }
    }
    outcome
}

/// Whether a Lua file with the given name is present in the plug-in folder, regardless of its
/// content. Used to tell "a (possibly wrong) Lua is installed" apart from "nothing installed".
#[must_use]
pub fn app_lua_present(steam_directory: &Path, lua_file_name: &str) -> bool {
    let safe = file_name_of(lua_file_name);
    !safe.is_empty() && plugin_directory(steam_directory).join(safe).is_file()
}

/// Returns true when every Lua unlock file for `files` is present and matches its blob SHA.
#[must_use]
pub fn has_app_lua_files(steam_directory: &Path, files: &[RepositoryFile]) -> bool {
    if !is_valid_steam_directory(Some(steam_directory)) {
        return false;
    }
    let target = plugin_directory(steam_directory);
    let lua: Vec<&RepositoryFile> = files
        .iter()
        .filter(|file| file.relative_path.to_ascii_lowercase().ends_with(".lua"))
        .collect();
    if lua.is_empty() {
        return false;
    }
    // De-duplicate by file name, keeping the first expected SHA.
    let mut expected: Vec<(String, String)> = Vec::new();
    for file in lua {
        let name = file.file_name().to_owned();
        if !expected
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&name))
        {
            expected.push((name, file.sha.clone()));
        }
    }
    expected.iter().all(|(name, sha)| {
        let path = target.join(name);
        fs::read(&path)
            .map(|content| matches_git_blob_sha(&content, sha))
            .unwrap_or(false)
    })
}

/// Downloads-then-installs helper: turns a per-app file list into a name→bytes payload.
///
/// `fetch` is normally [`crate::proxy::ProxyClient::fetch_file`]. The caller keeps
/// control of networking; this only enforces safe, unique file names before download.
pub fn build_app_payload<F, E>(
    files: &[RepositoryFile],
    mut fetch: F,
) -> Result<BTreeMap<String, Vec<u8>>, ServiceError>
where
    F: FnMut(&RepositoryFile) -> Result<Vec<u8>, E>,
    E: std::fmt::Display,
{
    let lua: Vec<&RepositoryFile> = files
        .iter()
        .filter(|file| file.relative_path.to_ascii_lowercase().ends_with(".lua"))
        .collect();
    if lua.is_empty() {
        return Err(ServiceError::NoLuaFiles);
    }
    let mut payload = BTreeMap::new();
    for file in lua {
        let name = file_name_of(&file.relative_path).to_owned();
        if name.is_empty() {
            return Err(ServiceError::InvalidLuaName);
        }
        if payload.contains_key(&name) {
            return Err(ServiceError::DuplicateLuaName(name));
        }
        let bytes = fetch(file).map_err(|error| ServiceError::Download(error.to_string()))?;
        payload.insert(name, bytes);
    }
    Ok(payload)
}

/// Stages, verifies, backs up, copies, and (optionally) commits a batch of files atomically.
fn install_files_transactionally<C>(
    target_directory: &Path,
    files: &BTreeMap<String, Vec<u8>>,
    files_to_remove: &[String],
    commit: Option<C>,
) -> Result<(), ServiceError>
where
    C: FnOnce() -> Result<(), ServiceError>,
{
    let transaction = TempTransaction::new()?;
    let staged = transaction.subdir("staged")?;
    let backup = transaction.subdir("backup")?;
    let mut replaced: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();

    let outcome = (|| -> Result<(), ServiceError> {
        for (name, content) in files {
            let staged_path = staged.join(name);
            fs::write(&staged_path, content)?;
            if sha256(&fs::read(&staged_path)?) != sha256(content) {
                return Err(ServiceError::StagingVerification(name.clone()));
            }
        }

        for (name, content) in files {
            let destination = target_directory.join(name);
            if destination.exists() {
                fs::copy(&destination, backup.join(name))?;
            }
            fs::copy(staged.join(name), &destination)?;
            replaced.push(name.clone());
            if !destination.is_file() || sha256(&fs::read(&destination)?) != sha256(content) {
                return Err(ServiceError::PostCopyVerification(name.clone()));
            }
        }

        for name in dedup_ignore_case(files_to_remove) {
            if files.contains_key(&name) {
                continue;
            }
            let safe = file_name_of(&name);
            if safe != name || safe.is_empty() {
                return Err(ServiceError::InvalidObsoleteName);
            }
            let destination = target_directory.join(safe);
            if !destination.is_file() {
                continue;
            }
            fs::copy(&destination, backup.join(safe))?;
            fs::remove_file(&destination)?;
            removed.push(safe.to_owned());
        }

        if let Some(commit) = commit {
            commit()?;
        }
        Ok(())
    })();

    if outcome.is_err() {
        roll_back(target_directory, &backup, replaced.iter().chain(removed.iter()));
    }
    outcome
}

fn roll_back<'a, I>(target_directory: &Path, backup_directory: &Path, names: I)
where
    I: Iterator<Item = &'a String>,
{
    let names: Vec<&String> = names.collect();
    for name in names.into_iter().rev() {
        let destination = target_directory.join(name);
        let original = backup_directory.join(name);
        if original.is_file() {
            let _ = fs::copy(&original, &destination);
        } else if destination.exists() {
            let _ = fs::remove_file(&destination);
        }
    }
}

fn read_installed_version(steam_directory: &Path) -> String {
    read_installed_marker(steam_directory)
        .map(|marker| marker.version)
        .unwrap_or_default()
}

fn read_installed_file_names(steam_directory: &Path) -> Vec<String> {
    read_installed_marker(steam_directory)
        .map(|marker| marker.files)
        .unwrap_or_default()
}

fn read_installed_marker(steam_directory: &Path) -> Option<Marker> {
    let directory = service_directory(steam_directory);
    for name in [MARKER_NAME, LEGACY_MARKER_NAME] {
        let path = directory.join(name);
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(marker) = serde_json::from_str::<Marker>(&text)
            && !marker.version.trim().is_empty()
        {
            return Some(marker);
        }
    }
    None
}

fn ensure_steam_directory(steam_directory: &Path) -> Result<(), ServiceError> {
    if is_valid_steam_directory(Some(steam_directory)) {
        Ok(())
    } else {
        Err(ServiceError::NotSteamDirectory)
    }
}

fn create_plugin_directory(steam_directory: &Path) -> Result<PathBuf, ServiceError> {
    let directory = plugin_directory(steam_directory);
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn write_atomically(temporary: &Path, destination: &Path, bytes: &[u8]) -> Result<(), ServiceError> {
    let result = (|| {
        fs::write(temporary, bytes)?;
        fs::rename(temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn file_name_of(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

fn dedup_ignore_case(values: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values {
        if !result.iter().any(|existing| existing.eq_ignore_ascii_case(value)) {
            result.push(value.clone());
        }
    }
    result
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// A self-cleaning temporary transaction directory under the system temp folder.
struct TempTransaction {
    root: PathBuf,
}

impl TempTransaction {
    fn new() -> Result<Self, ServiceError> {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        let unique: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let root = std::env::temp_dir().join("Drydock").join(unique);
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn subdir(&self, name: &str) -> Result<PathBuf, ServiceError> {
        let path = self.root.join(name);
        fs::create_dir_all(&path)?;
        Ok(path)
    }
}

impl Drop for TempTransaction {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("The selected folder is not a Steam folder because steam.exe is missing.")]
    NotSteamDirectory,
    #[error("{0} is empty. Installation was cancelled.")]
    EmptyFile(String),
    #[error("{0} is not a valid Windows DLL. Installation was cancelled.")]
    InvalidDll(String),
    #[error("No Lua file is available for this app.")]
    NoLuaFiles,
    #[error("The app data contains an invalid Lua filename.")]
    InvalidLuaName,
    #[error("The app data contains the duplicate Lua filename {0}.")]
    DuplicateLuaName(String),
    #[error("The obsolete service file list contains an invalid filename.")]
    InvalidObsoleteName,
    #[error("Staging verification failed for {0}.")]
    StagingVerification(String),
    #[error("Post-copy verification failed for {0}.")]
    PostCopyVerification(String),
    #[error("Post-delete verification failed for {0}.")]
    PostDeleteVerification(String),
    #[error("A payload file could not be downloaded: {0}")]
    Download(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mfb::compute_git_blob_sha;

    fn fake_steam_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("steam.exe"), b"MZfake").expect("steam.exe");
        fs::create_dir_all(dir.path().join("steamapps")).expect("steamapps");
        dir
    }

    fn make_package(version: &str, files: &[(&str, &[u8])]) -> SteamServicePackage {
        SteamServicePackage {
            version: version.to_owned(),
            files: files
                .iter()
                .map(|(name, content)| ((*name).to_owned(), content.to_vec()))
                .collect(),
        }
    }

    fn manifest_for(version: &str, package: &SteamServicePackage) -> SteamServiceManifest {
        SteamServiceManifest {
            version: version.to_owned(),
            files: package
                .files
                .iter()
                .map(|(name, content)| RepositoryFile {
                    relative_path: format!("Files/OST/{name}"),
                    source_url: format!("https://example/{name}"),
                    sha: compute_git_blob_sha(content),
                })
                .collect(),
        }
    }

    #[test]
    fn install_then_status_reports_current() {
        let steam = fake_steam_dir();
        let package = make_package(
            "1.0.0",
            &[("steamclient.dll", b"MZ\x00service"), ("cfg.txt", b"data")],
        );
        let manifest = manifest_for("1.0.0", &package);

        assert_eq!(
            service_status(steam.path(), &manifest).state,
            SteamServiceState::NotInstalled
        );

        install_service(steam.path(), &package).expect("install");
        // Service payload must land directly beside steam.exe, not in config/stplug-in.
        let root = steam.path();
        assert!(root.join("steamclient.dll").is_file());
        assert!(root.join(MARKER_NAME).is_file());
        assert!(
            !root
                .join("config")
                .join("stplug-in")
                .join("steamclient.dll")
                .exists()
        );
        assert_eq!(
            service_status(steam.path(), &manifest).state,
            SteamServiceState::Current
        );
    }

    #[test]
    fn status_detects_update_and_tampering() {
        let steam = fake_steam_dir();
        let package = make_package("1.0.0", &[("steamclient.dll", b"MZ\x00v1")]);
        install_service(steam.path(), &package).expect("install");

        let newer = manifest_for(
            "2.0.0",
            &make_package("2.0.0", &[("steamclient.dll", b"MZ\x00v2")]),
        );
        assert_eq!(
            service_status(steam.path(), &newer).state,
            SteamServiceState::UpdateAvailable
        );

        // Same version, but the on-disk file no longer matches the advertised SHA.
        let tampered = manifest_for(
            "1.0.0",
            &make_package("1.0.0", &[("steamclient.dll", b"MZ\x00different")]),
        );
        assert_eq!(
            service_status(steam.path(), &tampered).state,
            SteamServiceState::Error
        );
    }

    #[test]
    fn uninstall_removes_service_files_and_marker() {
        let steam = fake_steam_dir();
        let package = make_package("1.0.0", &[("steamclient.dll", b"MZ\x00v1"), ("cfg.txt", b"data")]);
        install_service(steam.path(), &package).expect("install");
        let plugin = steam.path().to_path_buf();
        assert!(plugin.join("steamclient.dll").is_file());

        let status = uninstall_service(steam.path()).expect("uninstall");
        assert_eq!(status.state, SteamServiceState::NotInstalled);
        assert!(!plugin.join("steamclient.dll").exists());
        assert!(!plugin.join("cfg.txt").exists());
        assert!(!plugin.join(MARKER_NAME).exists());

        // Uninstalling again is a harmless no-op.
        assert_eq!(
            uninstall_service(steam.path()).expect("uninstall again").state,
            SteamServiceState::NotInstalled
        );
    }

    #[test]
    fn install_rejects_invalid_dll_and_leaves_no_trace() {
        let steam = fake_steam_dir();
        let bad = make_package("1.0.0", &[("evil.dll", b"not-mz")]);
        assert!(matches!(
            install_service(steam.path(), &bad),
            Err(ServiceError::InvalidDll(_))
        ));
        assert!(!steam.path().join("evil.dll").exists());
    }

    #[test]
    fn install_removes_obsolete_service_files() {
        let steam = fake_steam_dir();
        let plugin = steam.path().to_path_buf();
        fs::write(plugin.join("OnlineFix.dll"), b"MZold").expect("obsolete");

        install_service(
            steam.path(),
            &make_package("1.0.0", &[("steamclient.dll", b"MZ\x00new")]),
        )
        .expect("install");
        assert!(!plugin.join("OnlineFix.dll").exists());
        assert!(plugin.join("steamclient.dll").is_file());
    }

    #[test]
    fn install_migrates_legacy_plugin_location() {
        let steam = fake_steam_dir();
        // An old install wrote the service into config/stplug-in, next to a per-app Lua
        // unlock that must survive the migration to the Steam root.
        let legacy = steam.path().join("config").join("stplug-in");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(legacy.join("steamclient.dll"), b"MZold").expect("old dll");
        fs::write(legacy.join("version.json"), b"{}").expect("old version");
        fs::write(legacy.join("1234.lua"), b"addappid(1234)").expect("lua");
        let old_marker = Marker {
            version: "0.9.0".to_owned(),
            installed_utc: String::new(),
            files: vec!["steamclient.dll".to_owned(), "version.json".to_owned()],
        };
        fs::write(
            legacy.join(MARKER_NAME),
            serde_json::to_vec_pretty(&old_marker).expect("marker json"),
        )
        .expect("old marker");

        install_service(
            steam.path(),
            &make_package(
                "1.0.0",
                &[("steamclient.dll", b"MZ\x00new"), ("version.json", b"{}")],
            ),
        )
        .expect("install");

        // New payload lands beside steam.exe.
        assert!(steam.path().join("steamclient.dll").is_file());
        // Legacy service files and marker are cleaned up …
        assert!(!legacy.join("steamclient.dll").exists());
        assert!(!legacy.join("version.json").exists());
        assert!(!legacy.join(MARKER_NAME).exists());
        // … but the per-app Lua unlock stays put.
        assert!(legacy.join("1234.lua").is_file());
    }

    #[test]
    fn add_and_remove_app_files_round_trip() {
        let steam = fake_steam_dir();
        let mut payload = BTreeMap::new();
        payload.insert("1234.lua".to_owned(), b"addappid(1234)".to_vec());
        payload.insert("1234_extra.lua".to_owned(), b"setManifestid".to_vec());

        assert_eq!(add_app_files(steam.path(), &payload).expect("add"), 2);
        let plugin = steam.path().join("config").join("stplug-in");
        assert!(plugin.join("1234.lua").is_file());

        let files: Vec<RepositoryFile> = payload
            .iter()
            .map(|(name, content)| RepositoryFile {
                relative_path: format!("Files/APPS/1234/{name}"),
                source_url: format!("https://example/{name}"),
                sha: compute_git_blob_sha(content),
            })
            .collect();
        assert!(has_app_lua_files(steam.path(), &files));

        let names: Vec<String> = payload.keys().cloned().collect();
        assert_eq!(remove_app_files(steam.path(), &names).expect("remove"), 2);
        assert!(!plugin.join("1234.lua").exists());
        assert!(!has_app_lua_files(steam.path(), &files));
    }

    #[test]
    fn add_app_rejects_path_traversal_names() {
        let steam = fake_steam_dir();
        let mut payload = BTreeMap::new();
        payload.insert("../escape.lua".to_owned(), b"x".to_vec());
        assert!(matches!(
            add_app_files(steam.path(), &payload),
            Err(ServiceError::InvalidLuaName)
        ));
    }

    #[test]
    fn build_app_payload_downloads_lua_only_and_rejects_duplicates() {
        let files = vec![
            RepositoryFile {
                relative_path: "Files/APPS/1/a.lua".into(),
                source_url: "u".into(),
                sha: "s".into(),
            },
            RepositoryFile {
                relative_path: "Files/APPS/1/readme.txt".into(),
                source_url: "u".into(),
                sha: "s".into(),
            },
        ];
        let payload = build_app_payload::<_, String>(&files, |file| Ok(file.file_name().as_bytes().to_vec()))
            .expect("payload");
        assert_eq!(payload.len(), 1);
        assert!(payload.contains_key("a.lua"));

        let dupes = vec![
            RepositoryFile {
                relative_path: "Files/APPS/1/x.lua".into(),
                source_url: "u".into(),
                sha: "s".into(),
            },
            RepositoryFile {
                relative_path: "Files/APPS/1/sub/x.lua".into(),
                source_url: "u".into(),
                sha: "s".into(),
            },
        ];
        assert!(matches!(
            build_app_payload::<_, String>(&dupes, |_| Ok(vec![1])),
            Err(ServiceError::DuplicateLuaName(_))
        ));
    }

    #[test]
    fn operations_require_a_valid_steam_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            install_service(dir.path(), &make_package("1.0.0", &[("a.dll", b"MZx")])),
            Err(ServiceError::NotSteamDirectory)
        ));
    }
}
