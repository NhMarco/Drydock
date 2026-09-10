//! Applying and checking per-app game fixes. Only the GitHub-sourced **Denuvo fix** remains (the
//! DepotBox "online fix" API path was removed): two halves applied together — `{appid}.lua`, a
//! build-locked unlock that replaces the normal token Lua in the plug-in folder
//! (`config/stplug-in/{appid}.lua`), and `{appid}.zip`, a game-folder overlay. Both files are
//! git-blob-SHA verified. `fix_status` reports whether exactly the fix Lua — and no other — sits in
//! the plug-in folder.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::Path;

use thiserror::Error;

use crate::mfb::DenuvoFix;
use crate::steam_service::{ServiceError, add_app_files, app_lua_present, has_app_lua_files};

/// Whether the build-locked Denuvo-fix Lua — and not a normal token Lua — is installed for an app.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixStatus {
    /// The fix Lua is installed and its content matches the repository — the fix is active.
    Applied,
    /// A Lua for this app is installed, but it is not the fix Lua (e.g. the normal token),
    /// which is incompatible with the fix.
    IncompatibleLua,
    /// No Lua for this app is installed yet.
    NotApplied,
}

/// Reports whether exactly the Denuvo-fix Lua is the one currently sitting in the plug-in folder,
/// so the UI can guarantee no other (wrong) Lua is installed for the app.
#[must_use]
pub fn fix_status(steam_directory: &Path, denuvo: &DenuvoFix) -> FixStatus {
    if has_app_lua_files(steam_directory, std::slice::from_ref(&denuvo.lua)) {
        FixStatus::Applied
    } else if app_lua_present(steam_directory, denuvo.lua.file_name()) {
        FixStatus::IncompatibleLua
    } else {
        FixStatus::NotApplied
    }
}

/// Applies a Denuvo fix: installs the build-locked Lua into the plug-in folder (replacing any other
/// Lua for the app) and extracts the reassembled zip into the game's install directory.
///
/// `fix_lua` is the already-downloaded, SHA-verified Lua; `zip_path` is the fully reassembled zip on
/// disk (a single part, or split parts concatenated in order). Returns the number of files extracted.
pub fn apply_denuvo_fix(
    steam_directory: &Path,
    install_dir: &Path,
    denuvo: &DenuvoFix,
    fix_lua: &[u8],
    zip_path: &Path,
) -> Result<usize, FixError> {
    if !install_dir.is_dir() {
        return Err(FixError::GameNotInstalled);
    }
    // 1. Replace the plug-in Lua with the build-locked fix Lua (transactional). add_app_files
    //    overwrites the app's `{appid}.lua`, so no other Lua for the app is left behind.
    let mut payload = BTreeMap::new();
    payload.insert(denuvo.lua.file_name().to_owned(), fix_lua.to_vec());
    add_app_files(steam_directory, &payload)?;

    // 2. Extract the game-folder files over the install directory (streamed from disk).
    extract_zip(File::open(zip_path)?, install_dir)
}

/// Extracts a zip into `target_dir`, overwriting existing files. Entry paths that would escape
/// the target directory (zip-slip: `..`, absolute paths, drive prefixes) are rejected via
/// [`enclosed_name`], so a malicious archive can never write outside the game folder.
fn extract_zip<R: Read + Seek>(reader: R, target_dir: &Path) -> Result<usize, FixError> {
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut written = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(FixError::UnsafeZipEntry(entry.name().to_owned()));
        };
        let destination = target_dir.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&destination)?;
        io::copy(&mut entry, &mut file)?;
        written += 1;
    }
    Ok(written)
}

#[derive(Debug, Error)]
pub enum FixError {
    #[error("The game is not installed, so the fix has no folder to apply to")]
    GameNotInstalled,
    #[error("The fix archive contains an unsafe path: {0}")]
    UnsafeZipEntry(String),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, content) in entries {
                writer.start_file(*name, options).expect("start");
                writer.write_all(content).expect("write");
            }
            writer.finish().expect("finish");
        }
        buffer
    }

    #[test]
    fn extracts_nested_files_and_overwrites() {
        let target = tempfile::tempdir().expect("tempdir");
        fs::write(target.path().join("steam_api64.dll"), b"original").expect("seed");
        let zip = zip_with(&[
            ("OnlineFix64.dll", b"fix"),
            ("Data/Managed/Assembly-CSharp.dll", b"patched"),
            ("steam_api64.dll", b"replaced"),
        ]);

        let written = extract_zip(io::Cursor::new(&zip), target.path()).expect("extract");
        assert_eq!(written, 3);
        assert_eq!(fs::read(target.path().join("OnlineFix64.dll")).unwrap(), b"fix");
        assert_eq!(
            fs::read(target.path().join("Data/Managed/Assembly-CSharp.dll")).unwrap(),
            b"patched"
        );
        // Existing file was overwritten.
        assert_eq!(
            fs::read(target.path().join("steam_api64.dll")).unwrap(),
            b"replaced"
        );
    }

    #[test]
    fn rejects_zip_slip_entries() {
        let target = tempfile::tempdir().expect("tempdir");
        let zip = zip_with(&[("../escape.dll", b"evil")]);
        assert!(matches!(
            extract_zip(io::Cursor::new(&zip), target.path()),
            Err(FixError::UnsafeZipEntry(_))
        ));
        // Nothing was written outside the target.
        assert!(!target.path().parent().unwrap().join("escape.dll").exists());
    }
}
