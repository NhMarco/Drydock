//! App unlocks (a Lua plus depot manifests) beyond the plain add: installing files the user supplies,
//! and keeping unlocks that follow the latest version current. Independent of egui rendering.
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use drydock_core::{AppPayloadStore, OwnUnlock, add_app_files, install_depot_manifests, installed_app_luas};

/// Coordinates unlock writes into Steam between the user's own actions and the automatic updater.
///
/// Every write takes [`UnlockWrites::write`], so two writers never interleave. The UI also marks each
/// app the user changes; an update sweep leaves those alone, so it never replaces a choice the user
/// made while the sweep was running (adding the cracked version, say).
#[derive(Default)]
pub struct UnlockWrites {
    writes: Mutex<()>,
    touched: Mutex<HashSet<u32>>,
}

impl UnlockWrites {
    pub fn write(&self) -> MutexGuard<'_, ()> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that the user is changing `app_id`'s unlock.
    pub fn touch(&self, app_id: u32) {
        self.touched_apps().insert(app_id);
    }

    fn touched_apps(&self) -> MutexGuard<'_, HashSet<u32>> {
        self.touched.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What an update sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UnlockUpdateSweep {
    /// Apps whose Lua or manifests were replaced with the provider's current ones.
    pub updated: Vec<u32>,
    /// Apps that could not be checked or written.
    pub failed: usize,
}

/// The provider's current unlock for one app: the Lua and the manifests keyed by `depotcache` name.
pub type CurrentUnlock = (Vec<u8>, BTreeMap<String, Vec<u8>>);

/// Brings the unlock of each of `apps` to the provider's current one, where it changed.
///
/// `current` fetches an app's unlock (`Ok(None)` when the provider has no pinned Lua for it, which
/// leaves the app alone rather than downgrading it to an unpinned one). Apps are handled one at a
/// time, `spacing` apart, because each fetch can make the provider build a package. An app whose Lua
/// is no longer in Steam was removed outside Drydock and is not brought back.
pub fn update_unlocks(
    steam_root: &Path,
    store: &AppPayloadStore,
    apps: &[u32],
    writes: &UnlockWrites,
    spacing: Duration,
    current: impl Fn(u32) -> Result<Option<CurrentUnlock>, String>,
) -> UnlockUpdateSweep {
    writes.touched_apps().clear();
    let mut sweep = UnlockUpdateSweep::default();
    for (index, &app_id) in apps.iter().enumerate() {
        if index > 0 {
            std::thread::sleep(spacing);
        }
        let (lua, manifests) = match current(app_id) {
            Ok(Some(unlock)) => unlock,
            Ok(None) => continue,
            Err(_) => {
                sweep.failed += 1;
                continue;
            }
        };
        if !store.differs_from(app_id, &lua, &manifests) {
            continue;
        }
        let _write = writes.write();
        if writes.touched_apps().contains(&app_id) || !installed_app_luas(steam_root).contains(&app_id) {
            continue;
        }
        let name = format!("{app_id}.lua");
        let installed = add_app_files(steam_root, &BTreeMap::from([(name.clone(), lua.clone())]))
            .and_then(|_| install_depot_manifests(steam_root, &manifests));
        match installed {
            Ok(_) => {
                // Steam already has the new files; a failed copy only costs the offline reinstall.
                let _ = store.save(app_id, Some((name.as_str(), lua.as_slice())), &manifests);
                sweep.updated.push(app_id);
            }
            Err(_) => sweep.failed += 1,
        }
    }
    sweep
}

/// Installs files the user picked and keeps a copy in Drydock's store, returning the status note.
///
/// The store copy is merged with what was stored before: adding a few manifests keeps the Lua, and
/// a new Lua keeps the manifests.
pub fn install_own_unlock(
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
            " Drydock could not keep its own copy ({error}), so reinstalling it will need the files again."
        ),
    };

    let what = match (unlock.lua.is_some(), manifests) {
        (true, 0) => "your Lua".to_owned(),
        (true, count) => format!("your Lua and {count} depot manifest(s)"),
        (false, count) => format!("{count} depot manifest(s)"),
    };
    Ok(format!(
        "Added {what} for \"{name}\" to Steam. Automatic updates leave these files alone.{backup}"
    ))
}

/// The picked files as paths, in the order the dialog returned them.
pub fn pick_unlock_files() -> Option<Vec<PathBuf>> {
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
    fn only_changed_unlocks_still_in_steam_and_untouched_by_the_user_are_updated() {
        let steam = steam();
        let data = tempfile::tempdir().unwrap();
        let store = AppPayloadStore::new(data.path());
        let plugin = steam.path().join("config/stplug-in");
        for app in [1, 2, 3] {
            std::fs::write(plugin.join(format!("{app}.lua")), b"old").unwrap();
            store
                .save(
                    app,
                    Some((&format!("{app}.lua"), b"old")),
                    &manifests(&["10_1.manifest"]),
                )
                .unwrap();
        }
        // App 4 was removed from Steam by hand; app 5 has no pinned Lua upstream; app 6 fails.
        let writes = UnlockWrites::default();
        let sweep = update_unlocks(
            steam.path(),
            &store,
            &[1, 2, 3, 4, 5, 6],
            &writes,
            Duration::ZERO,
            |app| match app {
                1 => Ok(Some((b"new".to_vec(), manifests(&["10_2.manifest"])))),
                2 => Ok(Some((b"old".to_vec(), manifests(&["10_1.manifest"])))),
                3 => {
                    // The user adds the cracked version while the sweep runs.
                    writes.touch(3);
                    Ok(Some((b"new".to_vec(), manifests(&["10_2.manifest"]))))
                }
                4 => Ok(Some((b"new".to_vec(), BTreeMap::new()))),
                5 => Ok(None),
                _ => Err("provider down".to_owned()),
            },
        );
        assert_eq!(
            sweep,
            UnlockUpdateSweep {
                updated: vec![1],
                failed: 1
            }
        );
        assert_eq!(std::fs::read(plugin.join("1.lua")).unwrap(), b"new");
        assert!(steam.path().join("depotcache/10_2.manifest").is_file());
        assert_eq!(store.stored_lua(1).unwrap().1, b"new");
        assert_eq!(std::fs::read(plugin.join("3.lua")).unwrap(), b"old");
        assert!(!plugin.join("4.lua").exists());
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
