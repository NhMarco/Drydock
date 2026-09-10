//! Locating and vetting a foreign (non-Steam) game install folder before activation.
//!
//! Two jobs:
//! * [`resolve_game_root`] finds the exact install root inside a user-chosen folder by matching a
//!   Steam launch executable's *relative* path. Because that path and the activation payload zip
//!   are both relative to the same root, pinning it guarantees the zip extracts into the right
//!   place regardless of how the outer folder is named or nested.
//! * [`scan_crack_files`] / [`remove_paths`] find and delete known hypervisor/crack files that must
//!   not be present when activating.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use walkdir::WalkDir;

/// How deep to search a chosen folder for the launch executable.
const MAX_SEARCH_DEPTH: usize = 8;

/// File and folder names (matched case-insensitively, anywhere in the tree) that are hypervisor or
/// crack artifacts and must be removed before activation. Directories are removed recursively.
pub const CRACK_ARTIFACT_NAMES: &[&str] = &[
    // Folders
    "driver_intel",
    "driver_amd",
    // DLLs
    "DenuvOwO.dll",
    "0xZeOn.dll",
    "KIRIGIRI.dll",
    "reflex.dll",
    "hyperevade.dll",
    "hyperhv.dll",
    "version.dll",
    "winmm.dll",
    "cirno.dll",
    "cracksteam_api64.dll",
    "coldloader.dll",
    "dbdata.dll",
    "dinput8.dll",
];

/// Finds the game install root inside `chosen` by locating one of `executables` (Steam launch
/// executables, as relative paths like `bin64/Game.exe`). Searches, in order: `chosen` itself and
/// its parent as direct roots (the user may have picked one level too deep), then every descendant
/// directory of `chosen` (bounded depth) — so a differently named or nested folder still resolves.
/// Returns the directory the executable's full relative path resolves against.
#[must_use]
pub fn resolve_game_root(chosen: &Path, executables: &[String]) -> Option<PathBuf> {
    if !chosen.is_dir() {
        return None;
    }
    for executable in executables {
        let relative = normalize_relative(executable);
        if relative.as_os_str().is_empty() {
            continue;
        }
        // 1. The chosen folder or its parent directly contains the executable path.
        if chosen.join(&relative).is_file() {
            return Some(chosen.to_path_buf());
        }
        if let Some(parent) = chosen.parent()
            && parent.join(&relative).is_file()
        {
            return Some(parent.to_path_buf());
        }
        // 2. Search the subtree for the executable by file name, then derive and verify the root.
        let Some(file_name) = relative.file_name() else {
            continue;
        };
        for entry in WalkDir::new(chosen)
            .max_depth(MAX_SEARCH_DEPTH)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file()
                && entry.file_name().eq_ignore_ascii_case(file_name)
                && let Some(root) = strip_relative_suffix(entry.path(), &relative)
                && root.join(&relative).is_file()
            {
                return Some(root);
            }
        }
    }
    None
}

/// Scans `root` and every subfolder for known hypervisor/crack artifacts, returning the paths
/// found. A matched directory is returned as-is (its contents are not listed separately).
#[must_use]
pub fn scan_crack_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut skip_prefix: Option<PathBuf> = None;
    for entry in WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        // Once a whole artifact directory is matched, don't list files inside it.
        if let Some(prefix) = &skip_prefix {
            if path.starts_with(prefix) {
                continue;
            }
            skip_prefix = None;
        }
        let name = entry.file_name().to_string_lossy();
        if CRACK_ARTIFACT_NAMES
            .iter()
            .any(|artifact| artifact.eq_ignore_ascii_case(name.as_ref()))
        {
            if entry.file_type().is_dir() {
                skip_prefix = Some(path.to_path_buf());
            }
            found.push(path.to_path_buf());
        }
    }
    found
}

/// Deletes the given files/directories (directories recursively). Missing paths are ignored;
/// the first real deletion error is returned.
pub fn remove_paths(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        let result = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        if let Err(error) = result
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
    }
    Ok(())
}

/// Normalizes a Steam launch path (`bin64/Game.exe` or `game\bin\win64\game.exe`) into a relative
/// `PathBuf` with only normal components — any `.`/`..`/root/prefix component makes it empty so it
/// can never escape the chosen folder.
fn normalize_relative(executable: &str) -> PathBuf {
    let unified = executable.replace('\\', "/");
    let mut relative = PathBuf::new();
    for segment in unified.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return PathBuf::new();
        }
        relative.push(segment);
    }
    relative
}

/// If `path` ends with the components of `relative`, returns the prefix directory before them.
fn strip_relative_suffix(path: &Path, relative: &Path) -> Option<PathBuf> {
    let path_components: Vec<Component> = path.components().collect();
    let relative_components: Vec<Component> = relative.components().collect();
    if relative_components.is_empty() || relative_components.len() > path_components.len() {
        return None;
    }
    let split = path_components.len() - relative_components.len();
    for (left, right) in path_components[split..].iter().zip(&relative_components) {
        let (Component::Normal(left), Component::Normal(right)) = (left, right) else {
            return None;
        };
        if !left.eq_ignore_ascii_case(right) {
            return None;
        }
    }
    let mut root = PathBuf::new();
    for component in &path_components[..split] {
        root.push(component.as_os_str());
    }
    Some(root)
}

/// Moves `target` aside to `<target>.bak` so the game's own file survives being overwritten.
/// Returns whether a backup was made.
///
/// Nothing happens in two cases. A `target` that doesn't exist has nothing to preserve. And a `.bak`
/// that is already there is left strictly alone: it holds the genuine original, whereas the file
/// sitting at `target` on a second run is the *previous crack's* output — backing that up would bury
/// the real original under a copy of a crack file and make the game unrecoverable.
pub fn back_up_before_overwrite(target: &Path) -> io::Result<bool> {
    if !target.exists() {
        return Ok(false);
    }
    let mut backup = target.as_os_str().to_owned();
    backup.push(".bak");
    let backup = PathBuf::from(backup);
    if backup.exists() {
        return Ok(false);
    }
    fs::rename(target, &backup)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_original_file_is_moved_aside_before_being_overwritten() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("steam_api64.dll");
        fs::write(&target, b"original game file").expect("write");

        assert!(back_up_before_overwrite(&target).expect("backup"));
        assert!(!target.exists(), "the original is moved, not copied");
        let backup = directory.path().join("steam_api64.dll.bak");
        assert_eq!(fs::read(&backup).expect("read"), b"original game file");
    }

    #[test]
    fn a_second_crack_never_buries_the_real_original() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("steam_api64.dll");
        fs::write(&target, b"original game file").expect("write");
        back_up_before_overwrite(&target).expect("first backup");
        // What a first crack left behind, now being re-cracked.
        fs::write(&target, b"crack file").expect("write");

        assert!(!back_up_before_overwrite(&target).expect("second backup"));
        let backup = directory.path().join("steam_api64.dll.bak");
        assert_eq!(
            fs::read(&backup).expect("read"),
            b"original game file",
            "the genuine original must survive re-cracking"
        );
    }

    #[test]
    fn a_file_that_does_not_exist_yet_needs_no_backup() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("version.dll");
        assert!(!back_up_before_overwrite(&target).expect("backup"));
        assert!(!directory.path().join("version.dll.bak").exists());
    }

    #[test]
    fn resolves_root_from_nested_subfolder() {
        let dir = tempfile::tempdir().expect("tempdir");
        // chosen/CRIMSONDESERT/Crimson Desert/bin64/CrimsonDesert.exe
        let root = dir.path().join("CRIMSONDESERT").join("Crimson Desert");
        fs::create_dir_all(root.join("bin64")).expect("dirs");
        fs::write(root.join("bin64/CrimsonDesert.exe"), b"exe").expect("exe");
        let resolved = resolve_game_root(dir.path(), &["bin64/CrimsonDesert.exe".to_owned()]);
        assert_eq!(resolved.as_deref(), Some(root.as_path()));
    }

    #[test]
    fn resolves_root_when_chosen_is_one_level_too_deep() {
        let dir = tempfile::tempdir().expect("tempdir");
        // real root is `dir`; user picked `dir/bin64`
        fs::create_dir_all(dir.path().join("bin64")).expect("dirs");
        fs::write(dir.path().join("bin64/Game.exe"), b"exe").expect("exe");
        let resolved = resolve_game_root(&dir.path().join("bin64"), &["bin64/Game.exe".to_owned()]);
        assert_eq!(resolved.as_deref(), Some(dir.path()));
    }

    #[test]
    fn no_match_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("readme.txt"), b"x").expect("file");
        assert!(resolve_game_root(dir.path(), &["bin64/Game.exe".to_owned()]).is_none());
    }

    #[test]
    fn scan_and_remove_crack_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("winmm.dll"), b"x").expect("dll");
        fs::create_dir_all(dir.path().join("bin64/driver_intel")).expect("dir");
        fs::write(dir.path().join("bin64/driver_intel/config.ini"), b"x").expect("cfg");
        fs::write(dir.path().join("game.exe"), b"x").expect("keep");

        let found = scan_crack_files(dir.path());
        assert!(found.iter().any(|p| p.ends_with("winmm.dll")));
        assert!(found.iter().any(|p| p.ends_with("driver_intel")));
        // The file inside the matched directory is not listed separately.
        assert!(!found.iter().any(|p| p.ends_with("config.ini")));

        remove_paths(&found).expect("remove");
        assert!(!dir.path().join("winmm.dll").exists());
        assert!(!dir.path().join("bin64/driver_intel").exists());
        assert!(dir.path().join("game.exe").exists());
    }
}
