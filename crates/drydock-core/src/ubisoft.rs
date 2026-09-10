//! Ubisoft activation support.
//!
//! Unlike the Steam flow (which installs a signed, encrypted entitlement archive), Ubisoft
//! activation is a text hand-off wrapped in the *same* machine/App-bound signed-token machinery:
//!
//! 1. Drydock downloads the per-app **magicfiles** ZIP from the proxy and extracts it next to the
//!    game exe ([`install_magicfiles`]).
//! 2. Drydock launches the exe once; the magicfiles write a `token_req.txt` beside it, whose text is
//!    captured ([`run_and_capture_token_request`]).
//! 3. That text is embedded in a machine/App-bound activation request (see
//!    [`crate::activation::ActivationRequestService::generate_ubisoft_delivery_code`]).
//! 4. Staff answer with a token; the bot returns a signed response token whose payload is a ZIP
//!    containing a single `token.ini`. Installing it (the ordinary signed-token install path) drops
//!    `token.ini` next to the game exe.
//!
//! Only steps 1-2 are Ubisoft-specific; the request/response crypto is shared with Steam.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use thiserror::Error;

/// The file the magicfiles write next to the game exe on first launch.
pub const TOKEN_REQUEST_FILE: &str = "token_req.txt";
/// The file installed next to the game exe from the staff-issued response token.
pub const TOKEN_FILE: &str = "token.ini";

/// Magicfiles are small DRM helpers; cap the archive well below memory limits.
const MAX_MAGICFILES: usize = 10_000;
const MAX_MAGICFILES_BYTES: u64 = 512 * 1024 * 1024;
/// A `token_req.txt` is a short base64-ish blob (a few KB); refuse anything absurd. Kept modest so
/// the activation code that embeds it stays within the bot's `MAX_ACTIVATION_CODE_LENGTH`.
const MAX_TOKEN_REQUEST_BYTES: u64 = 16 * 1024;

#[derive(Debug, Error)]
pub enum UbisoftError {
    #[error("The game executable to launch was not found")]
    ExeMissing,
    #[error("The magicfiles archive contains an unsafe path: {0}")]
    UnsafePath(String),
    #[error("The magicfiles archive contains no files")]
    Empty,
    #[error("The magicfiles archive is too large")]
    TooLarge,
    #[error("The magicfiles archive contains too many files")]
    TooManyFiles,
    #[error("The game could not be launched: {0}")]
    Launch(#[source] std::io::Error),
    #[error(
        "The game did not produce a {TOKEN_REQUEST_FILE} in time. Launch it once so it can generate the token request, then try again."
    )]
    TokenRequestTimeout,
    #[error("The token request file was empty or too large")]
    InvalidTokenRequest,
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Extracts the magicfiles ZIP into `exe_dir` (the folder holding the game exe), overwriting any
/// existing files, with zip-slip protection. Returns the number of files written.
pub fn install_magicfiles(zip: &[u8], exe_dir: &Path) -> Result<usize, UbisoftError> {
    if !exe_dir.is_dir() {
        return Err(UbisoftError::ExeMissing);
    }
    let root = std::path::absolute(exe_dir)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip))?;
    if archive.len() > MAX_MAGICFILES {
        return Err(UbisoftError::TooManyFiles);
    }

    let mut written = 0_usize;
    let mut expanded: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else {
            return Err(UbisoftError::UnsafePath(entry.name().to_owned()));
        };
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAX_MAGICFILES_BYTES {
            return Err(UbisoftError::TooLarge);
        }
        let target = root.join(&relative);
        if !target.starts_with(&root) {
            return Err(UbisoftError::UnsafePath(entry.name().to_owned()));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&target)?;
        std::io::copy(&mut entry, &mut output)?;
        written += 1;
    }
    if written == 0 {
        return Err(UbisoftError::Empty);
    }
    Ok(written)
}

/// Removes any stale token request/response files beside the exe so the next launch is captured
/// cleanly and a previous run's `token.ini` cannot be mistaken for a fresh one. Best effort.
pub fn clear_previous_token_files(exe_dir: &Path) {
    for name in [TOKEN_REQUEST_FILE, TOKEN_FILE] {
        let _ = fs::remove_file(exe_dir.join(name));
    }
}

/// Launches `exe` once (from its own directory) and waits up to `timeout` for the magicfiles to
/// write `token_req.txt` beside it, returning that file's text. Any stale `token_req.txt` is
/// removed first so only a freshly generated request is captured. The game is left running (the
/// user closes it); on timeout the launched process is killed.
pub fn run_and_capture_token_request(exe: &Path, timeout: Duration) -> Result<String, UbisoftError> {
    if !exe.is_file() {
        return Err(UbisoftError::ExeMissing);
    }
    let dir = exe.parent().ok_or(UbisoftError::ExeMissing)?.to_path_buf();
    let request_path = dir.join(TOKEN_REQUEST_FILE);
    let _ = fs::remove_file(&request_path);

    let mut child = Command::new(exe)
        .current_dir(&dir)
        .spawn()
        .map_err(UbisoftError::Launch)?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(content) = read_stable_token_request(&request_path)? {
            return Ok(content);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(UbisoftError::TokenRequestTimeout);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Reads `token_req.txt` only once it exists and its size has settled between two samples (so a
/// half-written file is never captured). Returns `Ok(None)` while it is absent or still growing.
fn read_stable_token_request(path: &Path) -> Result<Option<String>, UbisoftError> {
    let Ok(first) = fs::metadata(path) else {
        return Ok(None);
    };
    if first.len() == 0 {
        return Ok(None);
    }
    if first.len() > MAX_TOKEN_REQUEST_BYTES {
        return Err(UbisoftError::InvalidTokenRequest);
    }
    std::thread::sleep(Duration::from_millis(300));
    let second = fs::metadata(path)?;
    if second.len() != first.len() || second.len() == 0 {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_TOKEN_REQUEST_BYTES {
        return Err(UbisoftError::InvalidTokenRequest);
    }
    let text = String::from_utf8_lossy(&bytes).trim().to_owned();
    if text.is_empty() {
        return Err(UbisoftError::InvalidTokenRequest);
    }
    Ok(Some(text))
}

/// The directory a captured `token.ini` / `token_req.txt` lives in for a resolved game exe.
#[must_use]
pub fn token_directory(exe: &Path) -> Option<PathBuf> {
    exe.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
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
    fn install_magicfiles_writes_files_beside_exe() {
        let dir = tempfile::tempdir().expect("dir");
        let count = install_magicfiles(
            &zip_with(&[("uplay_r1_loader64.dll", b"MZ"), ("cream_api.ini", b"[cfg]")]),
            dir.path(),
        )
        .expect("install");
        assert_eq!(count, 2);
        assert_eq!(fs::read(dir.path().join("uplay_r1_loader64.dll")).unwrap(), b"MZ");
        assert_eq!(fs::read(dir.path().join("cream_api.ini")).unwrap(), b"[cfg]");
    }

    #[test]
    fn install_magicfiles_rejects_zip_slip() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(matches!(
            install_magicfiles(&zip_with(&[("../escape.dll", b"evil")]), dir.path()),
            Err(UbisoftError::UnsafePath(_))
        ));
        assert!(!dir.path().parent().unwrap().join("escape.dll").exists());
    }

    #[test]
    fn install_magicfiles_rejects_empty_archive() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(matches!(
            install_magicfiles(&zip_with(&[]), dir.path()),
            Err(UbisoftError::Empty)
        ));
    }

    #[test]
    fn clear_previous_removes_stale_token_files() {
        let dir = tempfile::tempdir().expect("dir");
        fs::write(dir.path().join(TOKEN_REQUEST_FILE), b"old").expect("seed req");
        fs::write(dir.path().join(TOKEN_FILE), b"old").expect("seed ini");
        clear_previous_token_files(dir.path());
        assert!(!dir.path().join(TOKEN_REQUEST_FILE).exists());
        assert!(!dir.path().join(TOKEN_FILE).exists());
    }

    #[test]
    fn stable_read_waits_for_a_settled_nonempty_file() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(TOKEN_REQUEST_FILE);
        assert_eq!(read_stable_token_request(&path).expect("absent"), None);
        fs::write(&path, b"  ubi-token-request-blob  ").expect("write");
        assert_eq!(
            read_stable_token_request(&path).expect("settled"),
            Some("ubi-token-request-blob".to_owned())
        );
    }
}
