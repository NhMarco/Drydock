//! Unlock files the user supplies: a Lua and/or depot manifests picked from disk.
//!
//! Everything is checked before anything is installed. The Lua has to look like an unlock for the
//! app, and every manifest has to parse. Each manifest is renamed to the `<depot>_<manifest>.manifest`
//! name Steam looks up in `depotcache`, whatever the file was called on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::depot::DepotManifest;

/// A Lua is a few kilobytes of text; anything far larger is not one.
const MAXIMUM_LUA_BYTES: u64 = 4 * 1024 * 1024;
/// The same ceiling the proxy client applies to a whole depot package.
const MAXIMUM_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;

/// Checked unlock files for one app, ready to install.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnUnlock {
    pub app_id: u32,
    /// The Lua, installed as [`OwnUnlock::lua_file_name`].
    pub lua: Option<Vec<u8>>,
    /// Manifests keyed by their `depotcache` file name.
    pub manifests: BTreeMap<String, Vec<u8>>,
}

impl OwnUnlock {
    /// The plug-in folder name Steam and Drydock recognise the app's Lua by.
    #[must_use]
    pub fn lua_file_name(&self) -> String {
        format!("{}.lua", self.app_id)
    }
}

#[derive(Debug, Error)]
pub enum OwnUnlockError {
    #[error("Select a .lua file, one or more .manifest files, or both.")]
    NothingSelected,
    #[error("Only .lua and .manifest files can be added, not {0}.")]
    UnsupportedFile(String),
    #[error("Select a single .lua file per game.")]
    SeveralLuaFiles,
    #[error("{0} is too large to be an unlock file.")]
    TooLarge(String),
    #[error("{0} is not an unlock Lua: it has no addappid or setManifestid line.")]
    NotALua(String),
    #[error("{name} is not a valid depot manifest: {reason}")]
    InvalidManifest { name: String, reason: String },
    #[error(
        "Drydock could not tell which game these files belong to. Add them from the game's page, or \
         include a Lua that is named after its App ID."
    )]
    UnknownApp,
    #[error("The Lua is for App {found}, not App {expected}.")]
    LuaForAnotherApp { expected: u32, found: u32 },
    #[error("{name} could not be read: {source}")]
    Read {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

/// Reads and checks the picked `files`.
///
/// `app_id` is the game the files are being added for, when that is known (they were added from its
/// page). Otherwise it is taken from the Lua: its file name when that is an App ID, else its first
/// `addappid` line.
pub fn read_own_unlock(files: &[PathBuf], app_id: Option<u32>) -> Result<OwnUnlock, OwnUnlockError> {
    if files.is_empty() {
        return Err(OwnUnlockError::NothingSelected);
    }
    let mut lua: Option<(Vec<u8>, Option<u32>)> = None;
    let mut manifests = BTreeMap::new();
    for path in files {
        let name = display_name(path);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("lua") => {
                if lua.is_some() {
                    return Err(OwnUnlockError::SeveralLuaFiles);
                }
                let bytes = read_capped(path, &name, MAXIMUM_LUA_BYTES)?;
                if !crate::proxy::looks_like_lua(&bytes) {
                    return Err(OwnUnlockError::NotALua(name));
                }
                let named_app = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u32>().ok())
                    .filter(|id| *id > 0);
                lua = Some((bytes, named_app));
            }
            Some("manifest") => {
                let bytes = read_capped(path, &name, MAXIMUM_MANIFEST_BYTES)?;
                let manifest =
                    DepotManifest::parse(&bytes).map_err(|error| OwnUnlockError::InvalidManifest {
                        name: name.clone(),
                        reason: error.to_string(),
                    })?;
                manifests.insert(
                    format!("{}_{}.manifest", manifest.depot_id, manifest.manifest_gid),
                    bytes,
                );
            }
            _ => return Err(OwnUnlockError::UnsupportedFile(name)),
        }
    }

    let lua_apps = lua
        .as_ref()
        .map(|(bytes, _)| added_app_ids(bytes))
        .unwrap_or_default();
    let app_id = app_id
        .or_else(|| lua.as_ref().and_then(|(_, named_app)| *named_app))
        .or_else(|| lua_apps.first().copied())
        .ok_or(OwnUnlockError::UnknownApp)?;
    // A Lua that names apps has to name this one, or it unlocks a different game.
    if let Some(&found) = lua_apps.first()
        && !lua_apps.contains(&app_id)
    {
        return Err(OwnUnlockError::LuaForAnotherApp {
            expected: app_id,
            found,
        });
    }
    Ok(OwnUnlock {
        app_id,
        lua: lua.map(|(bytes, _)| bytes),
        manifests,
    })
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn read_capped(path: &Path, name: &str, maximum: u64) -> Result<Vec<u8>, OwnUnlockError> {
    let read_error = |source| OwnUnlockError::Read {
        name: name.to_owned(),
        source,
    };
    if std::fs::metadata(path).map_err(read_error)?.len() > maximum {
        return Err(OwnUnlockError::TooLarge(name.to_owned()));
    }
    std::fs::read(path).map_err(read_error)
}

/// The IDs of every `addappid(<id>…)` call outside a comment, in order.
fn added_app_ids(bytes: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(bytes);
    let mut ids = Vec::new();
    for line in text.lines() {
        let code = line.split("--").next().unwrap_or_default();
        let mut rest = code;
        while let Some(start) = rest.find("addappid(") {
            rest = &rest[start + "addappid(".len()..];
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(id) = digits.parse::<u32>()
                && id > 0
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depot::manifest::tests::build_manifest;

    const LUA: &[u8] =
        b"-- addappid(999)\naddappid(480)\naddappid(481, 1, \"00\")\nsetManifestid(481, \"7\")\n";

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn manifests_are_renamed_to_their_depotcache_name_and_the_app_comes_from_the_lua() {
        let dir = tempfile::tempdir().unwrap();
        let lua = write(dir.path(), "unlock.lua", LUA);
        let manifest = write(
            dir.path(),
            "whatever.manifest",
            &build_manifest(481, 77, false, &[]),
        );
        let unlock = read_own_unlock(&[lua, manifest], None).unwrap();
        assert_eq!(unlock.app_id, 480, "the commented-out line does not count");
        assert_eq!(unlock.lua_file_name(), "480.lua");
        assert_eq!(unlock.lua.as_deref(), Some(LUA));
        assert_eq!(unlock.manifests.keys().collect::<Vec<_>>(), ["481_77.manifest"]);
    }

    #[test]
    fn the_app_can_come_from_the_page_or_the_lua_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = write(dir.path(), "a.manifest", &build_manifest(10, 1, false, &[]));
        assert_eq!(
            read_own_unlock(std::slice::from_ref(&manifest), Some(7))
                .unwrap()
                .app_id,
            7
        );
        assert!(matches!(
            read_own_unlock(&[manifest], None),
            Err(OwnUnlockError::UnknownApp)
        ));
        let named = write(dir.path(), "480.lua", b"setManifestid(481, \"7\")");
        assert_eq!(read_own_unlock(&[named], None).unwrap().app_id, 480);
    }

    #[test]
    fn unusable_selections_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let lua = write(dir.path(), "480.lua", LUA);
        let other = write(dir.path(), "other.lua", LUA);
        let text = write(dir.path(), "notes.txt", b"hi");
        let not_lua = write(dir.path(), "empty.lua", b"print('hi')");
        let broken = write(dir.path(), "broken.manifest", b"not a manifest");
        assert!(matches!(
            read_own_unlock(&[], None),
            Err(OwnUnlockError::NothingSelected)
        ));
        assert!(matches!(
            read_own_unlock(&[lua.clone(), other], None),
            Err(OwnUnlockError::SeveralLuaFiles)
        ));
        assert!(matches!(
            read_own_unlock(&[text], Some(480)),
            Err(OwnUnlockError::UnsupportedFile(_))
        ));
        assert!(matches!(
            read_own_unlock(&[not_lua], Some(480)),
            Err(OwnUnlockError::NotALua(_))
        ));
        assert!(matches!(
            read_own_unlock(&[broken], Some(480)),
            Err(OwnUnlockError::InvalidManifest { .. })
        ));
        assert!(matches!(
            read_own_unlock(&[lua], Some(730)),
            Err(OwnUnlockError::LuaForAnotherApp {
                expected: 730,
                found: 480
            })
        ));
    }
}
