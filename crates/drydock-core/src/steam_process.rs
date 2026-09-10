use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use sysinfo::{ProcessesToUpdate, System};
use thiserror::Error;

pub fn restart_steam(root: &Path) -> Result<(), SteamProcessError> {
    stop_steam(root)?;
    thread::sleep(Duration::from_millis(500));
    start_steam(root)
}

/// Requests a clean Steam shutdown and waits for the process to exit.
///
/// The Steam Service payload beside `steam.exe` and the per-app files under
/// `config/stplug-in` are locked while Steam runs, so the Steam Service install and
/// per-app changes must stop Steam first.
pub fn stop_steam(root: &Path) -> Result<(), SteamProcessError> {
    let executable = steam_executable(root)?;
    if is_steam_running() {
        Command::new(&executable)
            .arg("-shutdown")
            .current_dir(root)
            .spawn()
            .map_err(|source| SteamProcessError::ShutdownRequest {
                executable: executable.clone(),
                source,
            })?;
        wait_for_steam_shutdown(Duration::from_secs(20))?;
    }
    Ok(())
}

/// Starts Steam from `root`.
pub fn start_steam(root: &Path) -> Result<(), SteamProcessError> {
    let executable = steam_executable(root)?;
    Command::new(&executable)
        .current_dir(root)
        .spawn()
        .map_err(|source| SteamProcessError::Start { executable, source })?;
    Ok(())
}

/// Returns true while a main Steam process is running.
#[must_use]
pub fn is_steam_running() -> bool {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .any(|process| is_steam_main_process(&process.name().to_string_lossy().to_ascii_lowercase()))
}

fn wait_for_steam_shutdown(timeout: Duration) -> Result<(), SteamProcessError> {
    let deadline = std::time::Instant::now() + timeout;
    while is_steam_running() {
        if std::time::Instant::now() >= deadline {
            return Err(SteamProcessError::ShutdownTimeout);
        }
        thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

fn is_steam_main_process(name: &str) -> bool {
    matches!(name, "steam.exe" | "steam" | "steam.sh" | "steam_osx")
}

fn steam_executable(root: &Path) -> Result<PathBuf, SteamProcessError> {
    #[cfg(target_os = "windows")]
    let candidates = [root.join("steam.exe")];

    #[cfg(target_os = "linux")]
    let candidates = [root.join("steam.sh"), root.join("ubuntu12_32").join("steam")];

    #[cfg(target_os = "macos")]
    let candidates = [
        root.join("Steam.AppBundle")
            .join("Steam")
            .join("Contents")
            .join("MacOS")
            .join("steam_osx"),
        root.join("Steam.app")
            .join("Contents")
            .join("MacOS")
            .join("steam_osx"),
    ];

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    let candidates: [PathBuf; 0] = [];

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| SteamProcessError::ExecutableNotFound(root.to_path_buf()))
}

#[derive(Debug, Error)]
pub enum SteamProcessError {
    #[error("Steam executable was not found below {0}")]
    ExecutableNotFound(PathBuf),
    #[error("Steam shutdown could not be requested through {executable}: {source}")]
    ShutdownRequest {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Steam did not close within 20 seconds. Close it manually and try again")]
    ShutdownTimeout,
    #[error("Steam could not be started from {executable}: {source}")]
    Start {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_known_steam_processes() {
        assert!(is_steam_main_process("steam.exe"));
        assert!(is_steam_main_process("steam_osx"));
        assert!(!is_steam_main_process("steamwebhelper"));
        assert!(!is_steam_main_process("not-steam.exe"));
    }

    #[test]
    fn missing_executable_is_reported() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            steam_executable(root.path()),
            Err(SteamProcessError::ExecutableNotFound(_))
        ));
    }
}
