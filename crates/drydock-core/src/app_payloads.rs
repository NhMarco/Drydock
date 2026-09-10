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

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
        let directory = self.app_directory(app_id);
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        fs::create_dir_all(&directory)?;
        if let Some((name, bytes)) = lua
            && is_safe_name(name)
        {
            fs::write(directory.join(name), bytes)?;
        }
        for (name, bytes) in manifests {
            if is_safe_name(name) {
                fs::write(directory.join(name), bytes)?;
            }
        }
        Ok(())
    }

    /// Reads back whatever is stored.
    ///
    /// Never fails: a missing or unreadable store yields an empty payload and the caller falls back
    /// to fetching, which is always possible when there is a network.
    #[must_use]
    pub fn load(&self, app_id: u32) -> StoredPayload {
        let mut payload = StoredPayload::default();
        let Ok(entries) = fs::read_dir(self.app_directory(app_id)) else {
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

    /// Deletes the stored payload for one app. Already absent counts as success.
    pub fn remove(&self, app_id: u32) -> io::Result<()> {
        match fs::remove_dir_all(self.app_directory(app_id)) {
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
            .expect("save");

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
