use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Per-user data locations. Persistent data (settings, activation device key) lives directly in
/// the data root; regenerable caches live in a `cache` subdirectory that can be wiped safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortablePaths {
    root: PathBuf,
}

impl PortablePaths {
    /// Resolves the per-user data directory — `%LOCALAPPDATA%\Drydock` on Windows,
    /// `$XDG_DATA_HOME/Drydock` (or `~/.local/share/Drydock`) elsewhere — and migrates a legacy
    /// portable `settings` folder next to the executable on first run. Falls back to a folder
    /// beside the executable when the platform data directory cannot be determined.
    pub fn discover() -> io::Result<Self> {
        let root = platform_data_root()
            .or_else(legacy_executable_dir)
            .ok_or_else(|| io::Error::other("could not determine a data directory"))?;
        let paths = Self { root };
        paths.migrate_legacy_data();
        Ok(paths)
    }

    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding persistent user data (settings and the activation device key).
    #[must_use]
    pub fn settings_dir(&self) -> PathBuf {
        self.root.clone()
    }

    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// Directory holding regenerable caches (game list, Denuvo list, store artwork and details).
    #[must_use]
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Deletes the entire cache directory, returning the number of bytes freed. Persistent data
    /// (settings, activation) is left untouched.
    pub fn clear_cache(&self) -> io::Result<u64> {
        let cache = self.cache_dir();
        let freed = directory_size(&cache);
        if cache.exists() {
            fs::remove_dir_all(&cache)?;
        }
        Ok(freed)
    }

    /// Copies a legacy portable `settings` folder (next to the executable) into the data root the
    /// first time the app runs against the new location, so existing users keep their settings
    /// and activation. Best-effort: failures leave the app to start fresh.
    fn migrate_legacy_data(&self) {
        if self.settings_file().exists() {
            return;
        }
        let Some(legacy) = legacy_executable_dir() else {
            return;
        };
        if legacy == self.root || !legacy.join("settings.json").is_file() {
            return;
        }
        let _ = copy_dir_recursive(&legacy, &self.root);
    }
}

#[cfg(windows)]
fn platform_data_root() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|base| PathBuf::from(base).join("Drydock"))
}

#[cfg(not(windows))]
fn platform_data_root() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })
        .map(|base| base.join("Drydock"))
}

/// The legacy portable location: a `settings` folder beside the executable.
fn legacy_executable_dir() -> Option<PathBuf> {
    env::current_exe().ok()?.parent().map(|dir| dir.join("settings"))
}

fn directory_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source, &destination)?;
        } else {
            fs::copy(&source, &destination)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_separate_from_settings_and_clearable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = PortablePaths::at(temp.path());

        // Persistent data lives in the root; caches live under `cache`.
        assert_eq!(paths.settings_file(), temp.path().join("settings.json"));
        assert_eq!(paths.cache_dir(), temp.path().join("cache"));

        fs::create_dir_all(paths.cache_dir()).expect("cache dir");
        fs::write(paths.settings_file(), b"{}").expect("settings");
        fs::write(paths.cache_dir().join("games.json"), b"0123456789").expect("cache file");

        let freed = paths.clear_cache().expect("clear");
        assert_eq!(freed, 10);
        assert!(!paths.cache_dir().exists());
        // Settings survive the cache clear.
        assert!(paths.settings_file().is_file());
        // Clearing again is a no-op that frees nothing.
        assert_eq!(paths.clear_cache().expect("clear again"), 0);
    }
}
