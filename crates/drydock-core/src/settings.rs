use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::AddedAppState;

/// One game Drydock has downloaded via the depot engine. This is the Library's source of truth now
/// that Drydock no longer reads Steam's `appmanifest.acf` files.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "PascalCase")]
pub struct InstalledGame {
    pub name: String,
    /// Absolute path to the game's install folder (`<games_directory>/<installdir>`).
    pub install_dir: String,
}

/// One depot download waiting in (or currently at the front of) the download queue. Persisted so an
/// unfinished download resumes automatically after Drydock is closed and reopened; the depot engine
/// resumes from the on-disk chunks, so only the App ID and name need saving.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "PascalCase")]
pub struct QueuedDownload {
    pub app_id: u32,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Settings {
    /// The Steam install directory, for the (restored) Steam integration — library discovery, the
    /// Steam Service, lua install, manifest locks. Empty when Steam isn't configured/used.
    pub steam_directory: String,
    /// Where Drydock downloads games (`<games_directory>/<installdir>`). Empty means "use the
    /// platform default" (resolved by the UI, e.g. `C:\Games` on Windows).
    pub games_directory: String,
    pub added_apps: BTreeMap<u32, AddedAppState>,
    pub auto_update_drydock: bool,
    /// Per-app Steam manifest update locks (App ID → updates enabled), for the Steam integration.
    pub steam_updates_enabled: BTreeMap<u32, bool>,
    /// Games downloaded by Drydock, keyed by App ID — the Library reads this.
    pub installed_games: BTreeMap<u32, InstalledGame>,
    /// Per-app launch executables, so a downloaded game can be started from the Drydock library.
    pub launch_paths: BTreeMap<u32, String>,
    /// Optional path to the Cold Client Loader skeleton ZIP (the shared emu DLLs) merged into
    /// generated emulator templates. Empty means only the config files are written.
    pub emu_skeleton_path: String,
    /// The depot download queue (front = current). Persisted so an unfinished download resumes
    /// automatically on the next launch.
    pub download_queue: Vec<QueuedDownload>,
    /// Maximum parallel CDN connections for a depot download (clamped to 1..=32 when used).
    #[serde(default = "default_max_connections")]
    pub max_download_connections: u32,
    /// Maximum aggregate download speed in MB/s (`0` = unlimited).
    pub max_download_mbps: u32,

    // --- Self-hosting overrides -------------------------------------------------------------
    // Empty means "use the value this build was compiled with". See `crate::config` for the full
    // resolution order; an environment variable still beats anything stored here.
    /// Base URL of the Drydock proxy to talk to, e.g. `https://proxy.example`.
    pub proxy_base_url: String,
    /// Shared HMAC secret matching one of that proxy's `DRYDOCK_HMAC_SECRET` values.
    pub proxy_hmac_secret: String,
    /// `owner/repo` the self-updater checks, for forks running their own release channel.
    pub update_repository: String,
}

impl Settings {
    /// The self-hosting overrides in the shape [`crate::config`] wants.
    #[must_use]
    pub fn config_overrides(&self) -> crate::config::UserOverrides {
        crate::config::UserOverrides {
            proxy_base_url: self.proxy_base_url.clone(),
            hmac_secret: self.proxy_hmac_secret.clone(),
            update_repository: self.update_repository.clone(),
        }
    }

    /// Pushes those overrides into the process-wide config layer. Call after loading and after any
    /// edit, so the next proxy request uses the new address without a restart.
    pub fn apply_config_overrides(&self) {
        crate::config::set_user_overrides(self.config_overrides());
    }
}

fn default_max_connections() -> u32 {
    8
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            steam_directory: String::new(),
            games_directory: String::new(),
            added_apps: BTreeMap::new(),
            auto_update_drydock: true,
            steam_updates_enabled: BTreeMap::new(),
            installed_games: BTreeMap::new(),
            launch_paths: BTreeMap::new(),
            emu_skeleton_path: String::new(),
            download_queue: Vec::new(),
            max_download_connections: 8,
            max_download_mbps: 0,
            proxy_base_url: String::new(),
            proxy_hmac_secret: String::new(),
            update_repository: String::new(),
        }
    }
}

/// The outcome of [`Settings::load_recovering`]: the settings to use plus how they were obtained.
///
/// Anything other than [`LoadOutcome::Loaded`] means the caller must **not** silently overwrite the
/// on-disk file — a save would turn a recoverable problem into real data loss (the added apps,
/// installed games and launch paths are only stored there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    /// The settings file parsed cleanly (or did not exist yet, which is the normal first run).
    Loaded,
    /// The main file was unreadable/corrupt but the `.bak` written by the previous save parsed, so
    /// these settings are the last known-good state.
    RecoveredFromBackup { reason: String },
    /// Neither the main file nor the backup could be parsed. The unreadable file was moved aside to
    /// `quarantined` so nothing is lost, and these settings are the defaults.
    Quarantined {
        reason: String,
        quarantined: std::path::PathBuf,
    },
    /// The file exists but could not even be read (locked, permissions). Nothing was moved; the
    /// caller must treat the settings as read-only so the real data survives.
    Unreadable { reason: String },
}

impl LoadOutcome {
    /// Whether persisting over the settings file is safe. False for every recovery path except a
    /// backup restore (where the main file is already unusable, so rewriting it is the repair).
    #[must_use]
    pub fn save_is_safe(&self) -> bool {
        matches!(self, Self::Loaded | Self::RecoveredFromBackup { .. })
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path).map_err(|source| SettingsError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| SettingsError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Loads the settings, falling back to the `.bak` copy and finally to quarantining a corrupt
    /// file, so a bad `settings.json` can never cause the next save to wipe the user's library.
    ///
    /// [`Settings::save`] swaps the old file to `settings.json.bak` before moving the new one into
    /// place, which leaves two windows where the main file is missing or partial (a crash between
    /// the two renames, or a half-written temp file on a full disk). Plain [`Settings::load`] would
    /// report default settings for both, and the next save would then overwrite the only remaining
    /// copy of `added_apps` / `installed_games` / `launch_paths`. This resolves those cases instead
    /// and tells the caller, via [`LoadOutcome`], whether saving is safe.
    pub fn load_recovering(path: &Path) -> (Self, LoadOutcome) {
        if !path.exists() {
            // A missing main file with a usable backup means we crashed mid-swap in `save`.
            let backup = backup_path(path);
            if backup.exists()
                && let Some(settings) = read_parsed(&backup)
            {
                return (
                    settings,
                    LoadOutcome::RecoveredFromBackup {
                        reason: format!(
                            "{} was missing; restored the last good copy from {}",
                            path.display(),
                            backup.display()
                        ),
                    },
                );
            }
            return (Self::default(), LoadOutcome::Loaded);
        }

        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(source) => {
                // Cannot even read it — never overwrite, the real data is probably still in there.
                return (
                    Self::default(),
                    LoadOutcome::Unreadable {
                        reason: format!("{} could not be read: {source}", path.display()),
                    },
                );
            }
        };
        let parse_error = match serde_json::from_slice::<Self>(&bytes) {
            Ok(settings) => return (settings, LoadOutcome::Loaded),
            Err(error) => error,
        };

        let backup = backup_path(path);
        if let Some(settings) = read_parsed(&backup) {
            return (
                settings,
                LoadOutcome::RecoveredFromBackup {
                    reason: format!(
                        "{} was corrupt ({parse_error}); restored the last good copy from {}",
                        path.display(),
                        backup.display()
                    ),
                },
            );
        }

        // Nothing parsed. Move the corrupt file aside (never delete it — it may be hand-repairable)
        // and report that saving must stay blocked until the user acknowledges.
        match quarantine(path) {
            Ok(quarantined) => (
                Self::default(),
                LoadOutcome::Quarantined {
                    reason: format!("{} was corrupt ({parse_error})", path.display()),
                    quarantined,
                },
            ),
            Err(source) => (
                Self::default(),
                LoadOutcome::Unreadable {
                    reason: format!(
                        "{} was corrupt ({parse_error}) and could not be moved aside: {source}",
                        path.display()
                    ),
                },
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let parent = path
            .parent()
            .ok_or_else(|| SettingsError::InvalidPath(path.to_path_buf()))?;
        fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
            path: parent.to_path_buf(),
            source,
        })?;

        let temporary = path.with_extension("json.new");
        let bytes = serde_json::to_vec_pretty(self).map_err(SettingsError::Serialize)?;
        let mut output = fs::File::create(&temporary).map_err(|source| SettingsError::Write {
            path: temporary.clone(),
            source,
        })?;
        output.write_all(&bytes).map_err(|source| SettingsError::Write {
            path: temporary.clone(),
            source,
        })?;
        output.sync_all().map_err(|source| SettingsError::Write {
            path: temporary.clone(),
            source,
        })?;
        replace_file(&temporary, path)?;
        Ok(())
    }
}

/// The `.bak` companion of a settings file — the previous contents, kept after every successful
/// save so [`Settings::load_recovering`] has a known-good copy to fall back on.
fn backup_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("json.bak")
}

/// Reads and parses a settings file, returning `None` for anything unreadable or malformed.
fn read_parsed(path: &Path) -> Option<Settings> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Moves an unparseable settings file to `settings.corrupt-<n>.json` beside it, returning the new
/// path. Never overwrites an existing quarantine file, so repeated bad starts each keep their copy.
fn quarantine(path: &Path) -> Result<std::path::PathBuf, std::io::Error> {
    let stem = path
        .file_stem()
        .map_or_else(|| "settings".into(), |s| s.to_string_lossy());
    let parent = path.parent().unwrap_or(Path::new("."));
    for index in 0..100 {
        let candidate = parent.join(format!("{stem}.corrupt-{index}.json"));
        if candidate.exists() {
            continue;
        }
        fs::rename(path, &candidate)?;
        return Ok(candidate);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many quarantined settings files",
    ))
}

/// Atomically swaps `temporary` into `destination`, keeping the previous contents as the `.bak`
/// copy. The backup is deliberately **left in place** after a successful swap: it is what
/// [`Settings::load_recovering`] restores from when the main file is later found corrupt or missing
/// (e.g. the process died between the two renames below, which on Windows cannot be done as one
/// atomic operation).
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), SettingsError> {
    if !destination.exists() {
        return fs::rename(temporary, destination).map_err(|source| SettingsError::Write {
            path: destination.to_path_buf(),
            source,
        });
    }

    let backup = backup_path(destination);
    if backup.exists() {
        fs::remove_file(&backup).map_err(|source| SettingsError::Write {
            path: backup.clone(),
            source,
        })?;
    }
    fs::rename(destination, &backup).map_err(|source| SettingsError::Write {
        path: destination.to_path_buf(),
        source,
    })?;
    if let Err(source) = fs::rename(temporary, destination) {
        let _ = fs::rename(&backup, destination);
        return Err(SettingsError::Write {
            path: destination.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("cannot read settings at {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse settings at {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("cannot serialize settings: {0}")]
    Serialize(serde_json::Error),
    #[error("cannot write settings at {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid settings path: {0}")]
    InvalidPath(std::path::PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_existing_dotnet_property_names() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            r#"{
              "GamesDirectory": "D:/Games",
              "AddedApps": { "111300": { "Files": { "a.lua": "ABC" } } },
              "AutoUpdateDrydock": false
            }"#,
        )
        .expect("write settings");

        let settings = Settings::load(&path).expect("load settings");
        assert!(!settings.auto_update_drydock);
        assert!(settings.added_apps.contains_key(&111_300));
        assert_eq!(settings.games_directory, "D:/Games");
    }

    #[test]
    fn saves_atomically_and_round_trips() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("settings").join("settings.json");
        let settings = Settings {
            games_directory: "D:/Games".into(),
            ..Settings::default()
        };
        settings.save(&path).expect("save settings");
        settings.save(&path).expect("replace settings");
        let loaded = Settings::load(&path).expect("load settings");
        assert_eq!(loaded.games_directory, "D:/Games");
        assert!(!path.with_extension("json.new").exists());
    }

    /// The backup must survive a successful save — it is the only copy `load_recovering` can fall
    /// back on when the main file is later found corrupt.
    #[test]
    fn save_keeps_the_previous_contents_as_a_backup() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("settings.json");
        Settings {
            games_directory: "D:/First".into(),
            ..Settings::default()
        }
        .save(&path)
        .expect("first save");
        Settings {
            games_directory: "D:/Second".into(),
            ..Settings::default()
        }
        .save(&path)
        .expect("second save");

        let backup = read_parsed(&backup_path(&path)).expect("backup parses");
        assert_eq!(backup.games_directory, "D:/First");
    }

    #[test]
    fn recovers_from_the_backup_when_the_main_file_is_corrupt() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("settings.json");
        let mut good = Settings::default();
        good.installed_games.insert(
            730,
            InstalledGame {
                name: "CS".into(),
                install_dir: "D:/Games/CS".into(),
            },
        );
        good.save(&path).expect("save good");
        good.save(&path).expect("save again so a backup exists");
        fs::write(&path, b"{ this is not json").expect("corrupt the main file");

        let (settings, outcome) = Settings::load_recovering(&path);
        assert!(matches!(outcome, LoadOutcome::RecoveredFromBackup { .. }));
        assert!(
            outcome.save_is_safe(),
            "rewriting a corrupt main file is the repair"
        );
        assert!(settings.installed_games.contains_key(&730));
    }

    /// The crash window in `replace_file`: the old file was already renamed to `.bak` but the new
    /// one was not moved into place yet. Plain `load` would report defaults here.
    #[test]
    fn recovers_from_the_backup_when_the_main_file_vanished_mid_swap() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("settings.json");
        Settings {
            games_directory: "D:/Games".into(),
            ..Settings::default()
        }
        .save(&path)
        .expect("save");
        fs::rename(&path, backup_path(&path)).expect("simulate a crash mid-swap");

        let (settings, outcome) = Settings::load_recovering(&path);
        assert!(matches!(outcome, LoadOutcome::RecoveredFromBackup { .. }));
        assert_eq!(settings.games_directory, "D:/Games");
    }

    #[test]
    fn quarantines_a_corrupt_file_with_no_usable_backup_and_blocks_saving() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("settings.json");
        fs::write(&path, b"not json at all").expect("write corrupt settings");

        let (settings, outcome) = Settings::load_recovering(&path);
        let LoadOutcome::Quarantined { quarantined, .. } = &outcome else {
            panic!("expected quarantine, got {outcome:?}");
        };
        assert!(
            quarantined.is_file(),
            "the corrupt file must be kept, not deleted"
        );
        assert!(!path.exists(), "the corrupt file was moved aside");
        assert!(
            !outcome.save_is_safe(),
            "saving must stay blocked so nothing is overwritten"
        );
        assert_eq!(settings.games_directory, "");
    }

    #[test]
    fn a_missing_settings_file_is_a_normal_first_run() {
        let directory = tempfile::tempdir().expect("tempdir");
        let (settings, outcome) = Settings::load_recovering(&directory.path().join("settings.json"));
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert!(outcome.save_is_safe());
        assert_eq!(settings.added_apps.len(), 0);
    }
}
