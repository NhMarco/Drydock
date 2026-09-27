//! App unlocks (a Lua plus depot manifests) beyond the plain add: installing files the user
//! supplies. Independent of egui rendering.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use drydock_core::{AppPayloadStore, OwnUnlock, add_app_files, install_depot_manifests};

/// Coordinates unlock writes into Steam between actions running side by side (adding one game
/// while another is removed, say): every write takes [`UnlockWrites::write`], so two writers never
/// interleave.
#[derive(Default)]
pub(crate) struct UnlockWrites {
    writes: Mutex<()>,
}

impl UnlockWrites {
    pub(crate) fn write(&self) -> MutexGuard<'_, ()> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Installs files the user picked and keeps a copy in Drydock's store, returning the status note.
///
/// The store copy is merged with what was stored before: adding a few manifests keeps the Lua, and
/// a new Lua keeps the manifests.
pub(crate) fn install_own_unlock(
    steam_root: &Path,
    store: &AppPayloadStore,
    unlock: &OwnUnlock,
    writes: &UnlockWrites,
    name: &str,
) -> Result<String, String> {
    let _write = writes.write();
    let lua_name = unlock.lua_file_name();
    if let Some(lua) = &unlock.lua {
        add_app_files(steam_root, &BTreeMap::from([(lua_name.clone(), lua.clone())]))
            .map_err(|error| error.to_string())?;
    }
    let manifests =
        install_depot_manifests(steam_root, &unlock.manifests).map_err(|error| error.to_string())?;

    let mut stored = store.load(unlock.app_id);
    let lua = unlock
        .lua
        .clone()
        .or_else(|| stored.lua.take().map(|(_, bytes)| bytes));
    stored.manifests.extend(unlock.manifests.clone());
    let backup = match store.save(
        unlock.app_id,
        lua.as_deref().map(|bytes| (lua_name.as_str(), bytes)),
        &stored.manifests,
    ) {
        Ok(()) => String::new(),
        Err(error) => format!(
            " {product} could not keep its own copy ({error}), so reinstalling it will need the files again.",
            product = crate::brand::BRAND.name
        ),
    };

    let what = match (unlock.lua.is_some(), manifests) {
        (true, 0) => "your Lua".to_owned(),
        (true, count) => format!("your Lua and {count} depot manifest(s)"),
        (false, count) => format!("{count} depot manifest(s)"),
    };
    Ok(format!("Added {what} for \"{name}\" to Steam.{backup}"))
}

/// The picked files as paths, in the order the dialog returned them.
pub(crate) fn pick_unlock_files() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Select a Lua and/or depot manifests")
        .add_filter("Unlock files", &["lua", "manifest"])
        .pick_files()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steam() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("steam.exe"), b"").unwrap();
        std::fs::create_dir_all(dir.path().join("config/stplug-in")).unwrap();
        dir
    }

    fn manifests(names: &[&str]) -> BTreeMap<String, Vec<u8>> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), b"manifest".to_vec()))
            .collect()
    }

    #[test]
    fn own_files_are_installed_and_merged_into_the_stored_copy() {
        let steam = steam();
        let data = tempfile::tempdir().unwrap();
        let store = AppPayloadStore::new(data.path());
        store
            .save(480, Some(("480.lua", b"stored")), &manifests(&["1_1.manifest"]))
            .unwrap();
        let writes = UnlockWrites::default();
        let only_manifests = OwnUnlock {
            app_id: 480,
            lua: None,
            manifests: manifests(&["2_2.manifest"]),
        };
        let note = install_own_unlock(steam.path(), &store, &only_manifests, &writes, "Game").unwrap();
        assert!(note.contains("1 depot manifest(s)"), "{note}");
        assert!(steam.path().join("depotcache/2_2.manifest").is_file());
        let stored = store.load(480);
        assert_eq!(stored.lua.unwrap().1, b"stored");
        assert_eq!(stored.manifests.len(), 2);

        let with_lua = OwnUnlock {
            app_id: 480,
            lua: Some(b"addappid(480)".to_vec()),
            manifests: BTreeMap::new(),
        };
        install_own_unlock(steam.path(), &store, &with_lua, &writes, "Game").unwrap();
        assert_eq!(
            std::fs::read(steam.path().join("config/stplug-in/480.lua")).unwrap(),
            b"addappid(480)"
        );
        assert_eq!(store.load(480).manifests.len(), 2);
    }
}
