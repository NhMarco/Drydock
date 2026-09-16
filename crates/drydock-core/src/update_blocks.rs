//! Lifts the Steam update blocks earlier Drydock versions set.
//!
//! Drydock used to block a game's Steam updates by making its `appmanifest_<id>.acf` read-only, so
//! Steam could not record a newer build. That is not needed: an unlock's Lua has no way to pull
//! updates, and Steam can only download a build whose depot manifests it has. The feature is gone,
//! and this undoes what it left behind so Steam can write those manifests again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::models::SteamManifest;

/// What one pass of [`release_update_blocks`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateBlockRelease {
    /// Apps whose manifest was read-only and is writable again.
    pub released: Vec<u32>,
    /// Apps whose manifest could not be checked or made writable, with the reason. They stay
    /// recorded, so the next pass tries again.
    pub failed: Vec<(u32, String)>,
}

#[derive(Debug, Error)]
pub enum UpdateBlockError {
    #[error("cannot inspect Steam manifest {path}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot make Steam manifest {path} writable: {source}")]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Steam manifest {path} is still read-only")]
    StillReadOnly { path: PathBuf },
}

/// Lifts the update blocks among `manifests`.
///
/// A manifest is released when it is read-only and either `recorded` lists its app as blocked — the
/// old per-app setting, which also noted every manifest Drydock found read-only — or the app has an
/// unlock Lua (`lua_apps`). Read-only manifests of other games are left alone: something other than
/// Drydock blocked those.
///
/// `recorded` shrinks as apps are dealt with. A blocked app whose manifest is not among `manifests`
/// (uninstalled, or on a library drive that is not connected) stays recorded for a later pass.
pub fn release_update_blocks(
    manifests: &[SteamManifest],
    lua_apps: &BTreeSet<u32>,
    recorded: &mut BTreeMap<u32, bool>,
) -> UpdateBlockRelease {
    // Apps recorded with updates allowed have nothing to undo.
    recorded.retain(|_, updates_enabled| !*updates_enabled);

    let mut release = UpdateBlockRelease::default();
    let mut settled = BTreeSet::new();
    let mut unsettled = BTreeSet::new();
    for manifest in manifests {
        let app_id = manifest.app_id;
        if !recorded.contains_key(&app_id) && !lua_apps.contains(&app_id) {
            continue;
        }
        let path = &manifest.manifest_path;
        let outcome = is_read_only(path).and_then(|read_only| {
            if read_only {
                make_writable(path).map(|()| true)
            } else {
                Ok(false)
            }
        });
        match outcome {
            Ok(released) => {
                if released {
                    release.released.push(app_id);
                }
                settled.insert(app_id);
            }
            Err(error) => {
                release.failed.push((app_id, error.to_string()));
                unsettled.insert(app_id);
            }
        }
    }
    // An app can sit in more than one library; it is only done once every copy is.
    for app_id in settled.difference(&unsettled) {
        recorded.remove(app_id);
    }
    release
}

fn is_read_only(path: &Path) -> Result<bool, UpdateBlockError> {
    let metadata = fs::metadata(path).map_err(|source| UpdateBlockError::Metadata {
        path: path.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // The block cleared every write bit; the owner's is the one that matters.
        Ok(metadata.permissions().mode() & 0o200 == 0)
    }

    #[cfg(not(unix))]
    Ok(metadata.permissions().readonly())
}

fn make_writable(path: &Path) -> Result<(), UpdateBlockError> {
    let metadata = fs::metadata(path).map_err(|source| UpdateBlockError::Metadata {
        path: path.to_path_buf(),
        source,
    })?;
    let mut permissions = metadata.permissions();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(permissions.mode() | 0o200);
    }

    // On Windows this only clears the read-only attribute; Unix is handled above.
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);

    fs::set_permissions(path, permissions).map_err(|source| UpdateBlockError::SetPermissions {
        path: path.to_path_buf(),
        source,
    })?;
    if is_read_only(path)? {
        return Err(UpdateBlockError::StillReadOnly {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(directory: &Path, app_id: u32, read_only: bool) -> SteamManifest {
        let path = directory.join(format!("appmanifest_{app_id}.acf"));
        fs::write(&path, "\"AppState\" {}").unwrap();
        if read_only {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions).unwrap();
        }
        SteamManifest {
            app_id,
            name: format!("App {app_id}"),
            install_dir_name: String::new(),
            library_path: directory.to_path_buf(),
            manifest_path: path,
            state_flags: 4,
            bytes_to_download: 0,
            bytes_downloaded: 0,
            bytes_to_stage: 0,
            bytes_staged: 0,
            size_on_disk: None,
        }
    }

    fn read_only(manifest: &SteamManifest) -> bool {
        is_read_only(&manifest.manifest_path).unwrap()
    }

    #[test]
    fn blocks_drydock_set_or_found_are_lifted_and_forgotten() {
        let directory = tempfile::tempdir().unwrap();
        // Blocked through the old toggle (an owned game without a Lua).
        let owned = manifest(directory.path(), 10, true);
        // Blocked after an activation or fix, with its unlock Lua installed.
        let unlocked = manifest(directory.path(), 20, true);
        // Recorded as blocked, but already writable again.
        let unblocked = manifest(directory.path(), 30, false);
        // Read-only, but neither recorded nor unlocked: not Drydock's doing.
        let foreign = manifest(directory.path(), 40, true);
        let manifests = [owned.clone(), unlocked.clone(), unblocked, foreign.clone()];
        let lua_apps = BTreeSet::from([20]);
        // 50 is recorded as blocked but not installed right now; 60 had updates allowed.
        let mut recorded = BTreeMap::from([(10, false), (30, false), (50, false), (60, true)]);

        let release = release_update_blocks(&manifests, &lua_apps, &mut recorded);

        assert_eq!(release.released, [10, 20]);
        assert!(release.failed.is_empty());
        assert!(!read_only(&owned));
        assert!(!read_only(&unlocked));
        assert!(read_only(&foreign), "a block Drydock did not set is left alone");
        assert_eq!(
            recorded,
            BTreeMap::from([(50, false)]),
            "only the app that is not installed is kept for later"
        );

        // A second pass has nothing left to do.
        let again = release_update_blocks(&manifests, &lua_apps, &mut recorded);
        assert_eq!(again, UpdateBlockRelease::default());

        // Let the temporary folder be removed.
        make_writable(&foreign.manifest_path).unwrap();
    }

    #[test]
    fn a_manifest_that_cannot_be_read_stays_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let mut gone = manifest(directory.path(), 70, false);
        gone.manifest_path = directory.path().join("missing.acf");
        let mut recorded = BTreeMap::from([(70, false)]);

        let release = release_update_blocks(&[gone], &BTreeSet::new(), &mut recorded);

        assert_eq!(release.failed.len(), 1);
        assert_eq!(release.failed[0].0, 70);
        assert_eq!(recorded, BTreeMap::from([(70, false)]));
    }
}
