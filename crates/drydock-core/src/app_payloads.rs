//! Drydock's own copy of each app's unlock payload: the Lua and its depot manifests.
//!
//! Steam deletes an app's manifests from `depotcache` when the app is uninstalled. Without a copy
//! of our own, reinstalling would mean fetching the depot package from the provider again — slow,
//! and impossible offline or while the upstream is down. So every successful add writes the payload
//! here as well, and installing restores it into Steam first.
//!
//! This lives in the **persistent** data directory, not under `cache/`: clearing the cache is
//! advertised as safe and fully regenerable, and wiping this would silently destroy the one copy
//! that survives a Steam uninstall.
//!
//! Layout is one directory per app:
//!
//! ```text
//! <data dir>/apps/3751260/3751260.lua
//! <data dir>/apps/3751260/3751261_6667172545766883229.manifest
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// A `.payload-*` staging directory untouched for this long belongs to a save that never finished.
const STALE_STAGING_AGE: Duration = Duration::from_secs(60 * 60);

/// How often a directory rename is retried while Windows refuses it (see [`rename_directory`]).
const RENAME_ATTEMPTS: u64 = 5;

/// Serialises everything that touches one app's payload, so a reader never looks between the two
/// renames of a swap. Different apps do not wait for each other: the UI thread reads one app's
/// payload while a background add may be saving another's.
fn lock_app(directory: &Path) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = LazyLock::new(Mutex::default);
    LOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(directory.to_owned())
        .or_default()
        .clone()
}

fn hold(lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Renames a directory, retrying briefly while Windows refuses because a file inside is still open —
/// typically an antivirus scanner looking at the files that were just written.
fn rename_directory(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied && attempt < RENAME_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50 * attempt));
            }
            result => return result,
        }
    }
}

/// What is stored for one app. Either half may be missing: an app can have a Lua and no packaged
/// manifests at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredPayload {
    /// `(file name, bytes)` of the unlock Lua.
    pub lua: Option<(String, Vec<u8>)>,
    /// Depot manifests keyed by their `depotcache` file name.
    pub manifests: BTreeMap<String, Vec<u8>>,
}

impl StoredPayload {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lua.is_none() && self.manifests.is_empty()
    }
}

/// The on-disk store, rooted at `<data dir>/apps`.
#[derive(Clone, Debug)]
pub struct AppPayloadStore {
    root: PathBuf,
}

impl AppPayloadStore {
    #[must_use]
    pub fn new(data_directory: &Path) -> Self {
        Self {
            root: data_directory.join("apps"),
        }
    }

    #[must_use]
    pub fn app_directory(&self, app_id: u32) -> PathBuf {
        self.root.join(app_id.to_string())
    }

    /// Where the previous generation sits while [`Self::save`] swaps in a new one.
    fn backup_directory(&self, app_id: u32) -> PathBuf {
        self.root.join(format!("{app_id}.bak"))
    }

    /// The directory to read: the app's own, or the previous generation when a save was interrupted
    /// between its two renames and left only that.
    fn readable_directory(&self, app_id: u32) -> PathBuf {
        let directory = self.app_directory(app_id);
        if directory.exists() {
            directory
        } else {
            self.backup_directory(app_id)
        }
    }

    /// Removes staging directories left by a save that never finished. Only ones untouched for
    /// [`STALE_STAGING_AGE`] go, so a save still running in another Drydock process is left alone.
    fn remove_stale_staging(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let stale = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".payload-"))
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > STALE_STAGING_AGE);
            if stale {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Whether anything usable is stored for this app.
    #[must_use]
    pub fn has(&self, app_id: u32) -> bool {
        !self.load(app_id).is_empty()
    }

    /// Writes the payload, replacing whatever was stored for this app.
    ///
    /// The directory is rebuilt from scratch so a manifest that is no longer part of the app's
    /// package cannot linger and be restored later.
    pub fn save(
        &self,
        app_id: u32,
        lua: Option<(&str, &[u8])>,
        manifests: &BTreeMap<String, Vec<u8>>,
    ) -> io::Result<()> {
        if lua.is_some_and(|(name, _)| !is_safe_name(name))
            || manifests.keys().any(|name| !is_safe_name(name))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unsafe payload file name",
            ));
        }
        let directory = self.app_directory(app_id);
        let lock = lock_app(&directory);
        let _guard = hold(&lock);
        fs::create_dir_all(&self.root)?;
        self.remove_stale_staging();
        crate::safe_path::ensure_no_links(&self.root, &directory)?;
        let backup = self.backup_directory(app_id);
        crate::safe_path::ensure_no_links(&self.root, &backup)?;
        if !directory.exists() && backup.exists() {
            rename_directory(&backup, &directory)?;
        }
        let staged = tempfile::Builder::new()
            .prefix(".payload-")
            .tempdir_in(&self.root)?;
        if let Some((name, bytes)) = lua {
            fs::write(staged.path().join(name), bytes)?;
        }
        for (name, bytes) in manifests {
            fs::write(staged.path().join(name), bytes)?;
        }
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        if directory.exists() {
            rename_directory(&directory, &backup)?;
        }
        if let Err(error) = rename_directory(staged.path(), &directory) {
            if backup.exists() {
                rename_directory(&backup, &directory)?;
            }
            return Err(error);
        }
        // The previous generation only mattered while the swap was underway: readers fall back to it
        // just when the directory itself is missing.
        let _ = fs::remove_dir_all(&backup);
        Ok(())
    }

    /// Reads back whatever is stored.
    ///
    /// Never fails: a missing or unreadable store yields an empty payload and the caller falls back
    /// to fetching, which is always possible when there is a network.
    #[must_use]
    pub fn load(&self, app_id: u32) -> StoredPayload {
        let lock = lock_app(&self.app_directory(app_id));
        let _guard = hold(&lock);
        let mut payload = StoredPayload::default();
        let Ok(entries) = fs::read_dir(self.readable_directory(app_id)) else {
            return payload;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| is_safe_name(name)) else {
                continue;
            };
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let lowered = name.to_ascii_lowercase();
            if lowered.ends_with(".manifest") {
                payload.manifests.insert(name.to_owned(), bytes);
            } else if lowered.ends_with(".lua") {
                payload.lua = Some((name.to_owned(), bytes));
            }
        }
        payload
    }

    /// Only the stored Lua, as `(file name, bytes)`, without reading any manifest.
    #[must_use]
    pub fn stored_lua(&self, app_id: u32) -> Option<(String, Vec<u8>)> {
        let lock = lock_app(&self.app_directory(app_id));
        let _guard = hold(&lock);
        let entries = fs::read_dir(self.readable_directory(app_id)).ok()?;
        entries.flatten().find_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            if !is_safe_name(&name) || !name.to_ascii_lowercase().ends_with(".lua") {
                return None;
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                return None;
            }
            Some((name, fs::read(entry.path()).ok()?))
        })
    }

    /// Whether `lua` and `manifests` differ from what is stored for the app. Manifests are compared by
    /// name only: a name carries the depot and manifest ID, so a new build always has a new name.
    #[must_use]
    pub fn differs_from(&self, app_id: u32, lua: &[u8], manifests: &BTreeMap<String, Vec<u8>>) -> bool {
        self.stored_lua(app_id).is_none_or(|(_, stored)| stored != lua)
            || !self.manifest_index(app_id).keys().eq(manifests.keys())
    }

    /// The stored manifests' file names and byte sizes, without reading a single manifest.
    ///
    /// This is the cheap half of keeping Steam's `depotcache` intact: comparing names and sizes only
    /// costs a directory listing, so it can run on a timer, while [`Self::load`] — which pulls
    /// megabytes into memory — is reserved for the rare case where something actually went missing.
    #[must_use]
    pub fn manifest_index(&self, app_id: u32) -> BTreeMap<String, u64> {
        let lock = lock_app(&self.app_directory(app_id));
        let _guard = hold(&lock);
        let mut index = BTreeMap::new();
        let Ok(entries) = fs::read_dir(self.readable_directory(app_id)) else {
            return index;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| is_safe_name(name)) else {
                continue;
            };
            if !name.to_ascii_lowercase().ends_with(".manifest") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_file() {
                index.insert(name.to_owned(), metadata.len());
            }
        }
        index
    }

    /// Deletes the stored payload for one app. Already absent counts as success.
    pub fn remove(&self, app_id: u32) -> io::Result<()> {
        let directory = self.app_directory(app_id);
        let lock = lock_app(&directory);
        let _guard = hold(&lock);
        let backup = self.backup_directory(app_id);
        if backup.exists() {
            fs::remove_dir_all(backup)?;
        }
        match fs::remove_dir_all(directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Total bytes held, for the Settings readout. Best effort.
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        directory_size(&self.root)
    }
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_size(&entry.path()),
            Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Payload file names come from remote data, so only plain file names are ever touched.
fn is_safe_name(name: &str) -> bool {
    crate::safe_path::is_safe_path_segment(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifests(pairs: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(name, bytes)| ((*name).to_owned(), (*bytes).to_vec()))
            .collect()
    }

    #[test]
    fn a_new_lua_or_manifest_set_counts_as_a_difference() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        let current = manifests(&[("1_2.manifest", b"abc")]);
        assert!(store.differs_from(480, b"lua", &current), "nothing stored yet");
        store
            .save(480, Some(("480.lua", b"lua")), &current)
            .expect("save");
        assert_eq!(
            store.stored_lua(480),
            Some(("480.lua".to_owned(), b"lua".to_vec()))
        );
        assert!(!store.differs_from(480, b"lua", &current));
        assert!(store.differs_from(480, b"newer lua", &current));
        assert!(store.differs_from(480, b"lua", &manifests(&[("1_3.manifest", b"abc")])));
        assert!(store.differs_from(480, b"lua", &BTreeMap::new()));
    }

    #[test]
    fn the_manifest_index_lists_names_and_sizes_without_the_lua() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        store
            .save(
                480,
                Some(("480.lua", b"addappid(480)")),
                &manifests(&[("1_2.manifest", b"abc"), ("1_3.manifest", b"defgh")]),
            )
            .expect("save");

        let index = store.manifest_index(480);
        assert_eq!(index.len(), 2, "the Lua does not belong in the manifest index");
        assert_eq!(index.get("1_2.manifest"), Some(&3));
        assert_eq!(index.get("1_3.manifest"), Some(&5));
    }

    #[test]
    fn the_manifest_index_of_an_unknown_app_is_empty() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(
            AppPayloadStore::new(directory.path())
                .manifest_index(7)
                .is_empty()
        );
    }

    #[test]
    fn saves_and_loads_a_full_payload() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        let files = manifests(&[
            ("3751261_6667172545766883229.manifest", b"raw-manifest"),
            ("228990_1.manifest", b"redist"),
        ]);

        store
            .save(3_751_260, Some(("3751260.lua", b"addappid(3751260)")), &files)
            .expect("save");

        let loaded = store.load(3_751_260);
        assert_eq!(
            loaded.lua,
            Some(("3751260.lua".to_owned(), b"addappid(3751260)".to_vec()))
        );
        assert_eq!(loaded.manifests, files);
        assert!(store.has(3_751_260));
    }

    #[test]
    fn an_unknown_app_yields_an_empty_payload_rather_than_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        assert!(store.load(999).is_empty());
        assert!(!store.has(999));
    }

    /// Steam deleting an app's manifests is the case this store exists for, so what is kept here
    /// must be a complete replacement rather than an accumulation of every payload ever seen.
    #[test]
    fn saving_replaces_a_previous_payload_completely() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        store
            .save(
                730,
                Some(("730.lua", b"old")),
                &manifests(&[("1_1.manifest", b"a")]),
            )
            .expect("first save");
        store
            .save(
                730,
                Some(("730.lua", b"new")),
                &manifests(&[("2_2.manifest", b"b")]),
            )
            .expect("second save");

        let loaded = store.load(730);
        assert_eq!(loaded.lua.expect("lua").1, b"new");
        assert_eq!(loaded.manifests.len(), 1, "the stale manifest must not linger");
        assert!(loaded.manifests.contains_key("2_2.manifest"));
    }

    #[test]
    fn a_lua_only_payload_round_trips() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        store
            .save(1, Some(("1.lua", b"addappid(1)")), &BTreeMap::new())
            .expect("save");
        let loaded = store.load(1);
        assert!(loaded.manifests.is_empty());
        assert!(loaded.lua.is_some());
        assert!(!loaded.is_empty());
    }

    #[test]
    fn remove_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        store
            .save(5, Some(("5.lua", b"x")), &BTreeMap::new())
            .expect("save");
        store.remove(5).expect("first remove");
        store.remove(5).expect("removing again is not an error");
        assert!(!store.has(5));
    }

    /// Names reach the store from remote data, so a path in one must never escape the app directory.
    #[test]
    fn refuses_to_write_names_that_are_not_plain_file_names() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        store
            .save(
                7,
                Some(("../escape.lua", b"x")),
                &manifests(&[("sub/evil.manifest", b"y"), ("C:1_1.manifest", b"z")]),
            )
            .expect_err("unsafe input must fail before changing the store");

        assert!(store.load(7).is_empty(), "nothing unsafe may be written");
        assert!(!directory.path().join("escape.lua").exists());
    }

    #[test]
    fn size_reports_what_is_stored() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = AppPayloadStore::new(directory.path());
        assert_eq!(store.size_bytes(), 0);
        store
            .save(
                9,
                Some(("9.lua", b"1234567890")),
                &manifests(&[("1_1.manifest", b"abc")]),
            )
            .expect("save");
        assert_eq!(store.size_bytes(), 13);
    }
}
