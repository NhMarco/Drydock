//! Applying and checking per-app game fixes. Only the GitHub-sourced **Denuvo fix** remains (the
//! DepotBox "online fix" API path was removed): two halves applied together — `{appid}.lua`, a
//! build-locked unlock that replaces the normal token Lua in the plug-in folder
//! (`config/stplug-in/{appid}.lua`), and `{appid}.zip`, a game-folder overlay. Both files are
//! git-blob-SHA verified. `fix_status` reports whether exactly the fix Lua — and no other — sits in
//! the plug-in folder.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};

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
    // Validate the entire archive before changing either installation. It is staged, and the originals
    // it replaces are backed up, inside the game folder: on the drive that holds the game rather than
    // the system drive, and under the antivirus exclusion users are told to add for that folder.
    let staged = tempfile::Builder::new()
        .prefix(".drydock-fix-")
        .tempdir_in(install_dir)?;
    let extracted = extract_zip(File::open(zip_path)?, staged.path())?;
    if extracted.files.is_empty() {
        return Err(io::Error::other("Fix archive contains no files").into());
    }
    let mut changes = crate::file_transaction::FileTransaction::backed_up_in_root(install_dir)?;
    let mut payload = BTreeMap::new();
    payload.insert(denuvo.lua.file_name().to_owned(), fix_lua.to_vec());
    let result = (|| -> Result<(), FixError> {
        for relative in &extracted.folders {
            changes.create_dir_all(&install_dir.join(relative))?;
        }
        // Replaced from what was extracted, not from what is still staged: a staged file that has
        // disappeared since (quarantined, say) fails the copy and rolls the fix back.
        for relative in &extracted.files {
            changes.replace(&staged.path().join(relative), &install_dir.join(relative))?;
        }
        // The Lua is the completion indicator and is committed last.
        add_app_files(steam_directory, &payload)?;
        Ok(())
    })();
    if let Err(error) = result {
        changes.rollback()?;
        return Err(error);
    }
    changes.commit();
    Ok(extracted.files.len())
}

/// What [`extract_zip`] wrote, as paths relative to its target folder.
struct Extracted {
    files: Vec<PathBuf>,
    /// Folder entries of the archive, which may be empty and so appear in no file's path.
    folders: Vec<PathBuf>,
}

/// Extracts a zip into `target_dir`, overwriting existing files. Entry paths that would escape the
/// target directory (zip-slip: `..`, absolute paths, drive prefixes) are rejected via
/// [`enclosed_name`], and every write goes through a [`WriteRoot`](crate::safe_path::WriteRoot), so
/// a malicious archive or a link planted in the folder can never direct a write outside it.
fn extract_zip<R: Read + Seek>(reader: R, target_dir: &Path) -> Result<Extracted, FixError> {
    let scope = crate::safe_path::WriteRoot::new(target_dir)?;
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut extracted = Extracted {
        files: Vec::new(),
        folders: Vec::new(),
    };
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(FixError::UnsafeZipEntry(entry.name().to_owned()));
        };
        let destination = target_dir.join(&relative);
        if entry.is_dir() {
            scope.create_dir_all(&destination)?;
            extracted.folders.push(relative);
            continue;
        }
        if let Some(parent) = destination.parent() {
            scope.create_dir_all(parent)?;
        }
        let mut file = scope.create(&destination)?;
        io::copy(&mut entry, &mut file)?;
        extracted.files.push(relative);
    }
    Ok(extracted)
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
    use std::fs;
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
        assert_eq!(written.files.len(), 3);
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
