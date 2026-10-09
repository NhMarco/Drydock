//! The cracked build: the exact game version a fix is made for.
//!
//! A fix's Lua pins that version with `setManifestid(<depot>, "<manifest gid>")` lines. Steam, given
//! the Lua, fetches those manifests itself. Drydock's own downloader needs the manifest files in
//! hand, and no depot source serves an older manifest by its ID — so they come from one of two
//! places: the fix folder, which may carry the `{depot}_{gid}.manifest` files of the build, or the
//! depot package the sources serve today, when the game has not been updated since the fix was made
//! and its current manifests are still the pinned ones. With neither, the cracked build cannot be
//! downloaded (Steam can still get it, through "Add cracked version to Steam").

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use crate::depot::download::is_shared_redistributable_depot;
use crate::depot::manifest::ManifestError;
use crate::depot::{DepotData, DepotDownloadError, DepotKeys, DepotManifest};
use crate::mfb::{DenuvoFix, RepositoryFile};
use crate::proxy::{ProxyClient, ProxyError};

/// The manifest each depot is pinned to by `setManifestid` lines, keyed by depot. Commented-out
/// lines don't count: providers ship disabled pins (`-- setManifestid(…)`) that pin nothing.
#[must_use]
pub fn pinned_manifests(lua: &str) -> BTreeMap<u32, u64> {
    let mut pins = BTreeMap::new();
    for line in lua.lines() {
        let code = line.split("--").next().unwrap_or_default();
        let Some(start) = code.find("setManifestid(") else {
            continue;
        };
        let arguments = &code[start + "setManifestid(".len()..];
        let arguments = arguments.split(')').next().unwrap_or_default();
        let mut parts = arguments
            .split(',')
            .map(|part| part.trim().trim_matches('"').trim());
        let depot = parts.next().and_then(|value| value.parse::<u32>().ok());
        let gid = parts.next().and_then(|value| value.parse::<u64>().ok());
        if let (Some(depot), Some(gid)) = (depot, gid)
            && depot != 0
            && gid != 0
        {
            pins.insert(depot, gid);
        }
    }
    pins
}

#[derive(Debug, Error)]
pub enum CrackedBuildError {
    #[error("The fix does not name the game version it is made for, so that version can't be downloaded.")]
    NotPinned,
    #[error(
        "The cracked version can't be downloaded yet: the game has been updated since the fix was made, \
         and {missing} of the {pinned} depot manifests of the cracked version are not in the fix folder. \
         Add the cracked version to Steam instead."
    )]
    BuildUnavailable { missing: usize, pinned: usize },
    #[error("The manifest {0} in the fix folder is not the build it is named after.")]
    WrongManifest(String),
    #[error("The fix carries no depot key for depot {0}.")]
    MissingKey(u32),
    #[error("Stopped before the download started.")]
    Cancelled,
    #[error(transparent)]
    Proxy(#[from] ProxyError),
    #[error(transparent)]
    Depot(#[from] DepotDownloadError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

/// Depot data for the cracked build of `app_id`, ready for the depot engine to download or verify.
///
/// `folder_manifests` is what the fix folder offers (`FixList::manifests`); `skip_depots` are depots
/// not to download (another OS's builds). Keys come from the fix Lua first, then the package.
pub fn cracked_depot_data(
    proxy: &ProxyClient,
    app_id: u32,
    fix: &DenuvoFix,
    folder_manifests: &[RepositoryFile],
    skip_depots: &BTreeSet<u32>,
    cancel: &AtomicBool,
) -> Result<DepotData, CrackedBuildError> {
    let stopped = || {
        if cancel.load(Ordering::Relaxed) {
            Err(CrackedBuildError::Cancelled)
        } else {
            Ok(())
        }
    };
    let lua = proxy.fetch_file(&fix.lua)?;
    let text = String::from_utf8_lossy(&lua).into_owned();
    let pins: BTreeMap<u32, u64> = pinned_manifests(&text)
        .into_iter()
        .filter(|(depot, _)| !skip_depots.contains(depot))
        .collect();
    if pins.is_empty() {
        return Err(CrackedBuildError::NotPinned);
    }
    let mut keys = DepotKeys::parse_lua(&text);

    let mut raw: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (depot, gid) in &pins {
        let name = manifest_name(*depot, *gid);
        if let Some(file) = folder_manifests
            .iter()
            .find(|file| file.file_name().eq_ignore_ascii_case(&name))
        {
            stopped()?;
            raw.insert(name, proxy.fetch_file(file)?);
        }
    }
    if raw.len() < pins.len() {
        // Not all in the fix folder: the package of today has them if the game is still on that build.
        stopped()?;
        let package = DepotData::fetch_cancellable(proxy, app_id, cancel).map_err(|error| match error {
            DepotDownloadError::Cancelled => CrackedBuildError::Cancelled,
            other => CrackedBuildError::Depot(other),
        })?;
        for (depot, gid) in &pins {
            let name = manifest_name(*depot, *gid);
            if !raw.contains_key(&name)
                && let Some(bytes) = package.raw_manifests.get(&name)
            {
                raw.insert(name, bytes.clone());
            }
        }
        keys.merge_from(package.keys);
    }
    stopped()?;
    build_depot_data(app_id, &pins, raw, keys, lua)
}

fn manifest_name(depot: u32, gid: u64) -> String {
    format!("{depot}_{gid}.manifest")
}

/// The network-free half of [`cracked_depot_data`]: checks that every pinned content depot has its
/// manifest — and that each manifest is the build it is named after — and decrypts the file names.
fn build_depot_data(
    app_id: u32,
    pins: &BTreeMap<u32, u64>,
    raw: BTreeMap<String, Vec<u8>>,
    keys: DepotKeys,
    lua: Vec<u8>,
) -> Result<DepotData, CrackedBuildError> {
    // A shared redistributable (Visual C++, DirectX…) a fix happens to pin is not part of the game.
    let missing = pins
        .iter()
        .filter(|(depot, gid)| {
            !raw.contains_key(&manifest_name(**depot, **gid)) && !is_shared_redistributable_depot(**depot)
        })
        .count();
    if missing > 0 {
        return Err(CrackedBuildError::BuildUnavailable {
            missing,
            pinned: pins.len(),
        });
    }
    let mut manifests = Vec::with_capacity(raw.len());
    for (name, bytes) in &raw {
        let mut manifest = DepotManifest::parse(bytes)?;
        if pins.get(&manifest.depot_id) != Some(&manifest.manifest_gid)
            || manifest_name(manifest.depot_id, manifest.manifest_gid) != *name
        {
            return Err(CrackedBuildError::WrongManifest(name.clone()));
        }
        let key = keys
            .get(manifest.depot_id)
            .ok_or(CrackedBuildError::MissingKey(manifest.depot_id))?;
        manifest.decrypt_filenames(key)?;
        manifests.push(manifest);
    }
    let data = DepotData {
        app_id,
        keys,
        manifests,
        raw_manifests: raw,
        lua: Some((format!("{app_id}.lua"), lua)),
    };
    if data.has_no_content() {
        return Err(CrackedBuildError::BuildUnavailable {
            missing: pins.len(),
            pinned: pins.len(),
        });
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_live_pins_count() {
        let lua = r#"-- STEAMTOOLS SHADOW PROTOCOL CONFIG
addappid(1328670)
addappid(1328671, 1, "ab")
setManifestid(1328671, "9466046803274430000", 0)
setManifestid(1328672,"123") -- a trailing comment
-- setManifestid(1328673, "456")
setManifestid(1328674, "0")
setManifestid(oops, "789")
  setManifestid( 1328675 , 77 )"#;
        let pins = pinned_manifests(lua);
        assert_eq!(
            pins.into_iter().collect::<Vec<_>>(),
            vec![
                (1_328_671, 9_466_046_803_274_430_000),
                (1_328_672, 123),
                (1_328_675, 77)
            ]
        );
    }

    #[test]
    fn a_missing_content_manifest_means_the_build_is_unavailable() {
        let pins = BTreeMap::from([(1_328_671, 5), (228_988, 6)]);
        match build_depot_data(
            1_328_670,
            &pins,
            BTreeMap::new(),
            DepotKeys::default(),
            Vec::new(),
        ) {
            Err(CrackedBuildError::BuildUnavailable { missing, pinned }) => {
                // The shared redistributable is not required.
                assert_eq!((missing, pinned), (1, 2));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_manifest_is_only_taken_for_the_build_it_is_named_after() {
        let pins = BTreeMap::from([(1_328_671, 5)]);
        let raw = BTreeMap::from([("1328671_5.manifest".to_owned(), b"not a manifest".to_vec())]);
        assert!(matches!(
            build_depot_data(1_328_670, &pins, raw, DepotKeys::default(), Vec::new()),
            Err(CrackedBuildError::Manifest(_))
        ));
    }
}
