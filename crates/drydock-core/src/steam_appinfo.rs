//! Fetches a Steam app's Windows launch executable paths from public `app_info`.
//!
//! Used to verify a foreign (non-Steam) game install: the launch executable's *relative* path
//! (e.g. `bin64/CrimsonDesert.exe`) is expressed relative to the game's install root — the same
//! root the activation payload zip extracts into — so locating that executable in a chosen folder
//! pins the exact extraction root. Source: the public SteamCMD.net mirror of Steam `app_info`
//! (`config.launch`), which needs no API key, so the client fetches it directly.

use std::io::Read;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::USER_AGENT;
use thiserror::Error;

use crate::version::user_agent;

const MAXIMUM_APPINFO_BYTES: u64 = 8 * 1024 * 1024;

/// Returns the distinct Windows launch executables for `app_id`, as forward-slash relative paths
/// (e.g. `bin64/CrimsonDesert.exe`). Non-Windows and non-`.exe` launch entries are dropped.
pub fn fetch_windows_executables(app_id: u32) -> Result<Vec<String>, SteamAppInfoError> {
    if app_id == 0 {
        return Err(SteamAppInfoError::InvalidAppId);
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .get(format!("https://api.steamcmd.net/v1/info/{app_id}"))
        .header(USER_AGENT, user_agent())
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAXIMUM_APPINFO_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAXIMUM_APPINFO_BYTES {
        return Err(SteamAppInfoError::TooLarge);
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(windows_executables(&json, app_id))
}

/// Extracts the Windows `.exe` launch paths from a SteamCMD.net `app_info` document. Kept separate
/// (and pure) so it can be unit-tested against a captured document.
#[must_use]
pub fn windows_executables(json: &serde_json::Value, app_id: u32) -> Vec<String> {
    let launch = &json["data"][app_id.to_string()]["config"]["launch"];
    let Some(entries) = launch.as_object() else {
        return Vec::new();
    };
    let mut executables: Vec<String> = Vec::new();
    for entry in entries.values() {
        let Some(raw) = entry["executable"].as_str() else {
            continue;
        };
        let executable = raw.trim();
        if executable.is_empty() {
            continue;
        }
        // Drop entries explicitly for another OS; a missing oslist is treated as usable.
        let oslist = entry["config"]["oslist"].as_str().unwrap_or_default();
        if !oslist.is_empty() && !oslist.to_ascii_lowercase().contains("windows") {
            continue;
        }
        let normalized = executable.replace('\\', "/");
        if !normalized.to_ascii_lowercase().ends_with(".exe") {
            continue; // skip macOS `.app` / Linux `.sh` launch entries
        }
        if !executables
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&normalized))
        {
            executables.push(normalized);
        }
    }
    executables
}

/// Fetches the app's target architecture from public app-info: `true` = 64-bit, `false` = 32-bit,
/// `None` when it can't be determined (the caller then asks the user). Used so the emulator cracker
/// can pick the right DLLs without the user selecting the game exe.
pub fn fetch_windows_arch(app_id: u32) -> Result<Option<bool>, SteamAppInfoError> {
    if app_id == 0 {
        return Err(SteamAppInfoError::InvalidAppId);
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .get(format!("https://api.steamcmd.net/v1/info/{app_id}"))
        .header(USER_AGENT, user_agent())
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAXIMUM_APPINFO_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAXIMUM_APPINFO_BYTES {
        return Err(SteamAppInfoError::TooLarge);
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(windows_arch(&json, app_id))
}

/// Determines an app's Windows architecture from a SteamCMD.net app-info document: first the launch
/// entries' `config.osarch` (`"64"`/`"32"`), then a heuristic on the exe paths (`win64`/`bin64`/`x64`
/// vs `win32`/`x86`). Pure, for unit testing.
#[must_use]
pub fn windows_arch(json: &serde_json::Value, app_id: u32) -> Option<bool> {
    if let Some(entries) = json["data"][app_id.to_string()]["config"]["launch"].as_object() {
        for entry in entries.values() {
            let oslist = entry["config"]["oslist"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !oslist.is_empty() && !oslist.contains("windows") {
                continue;
            }
            match entry["config"]["osarch"].as_str() {
                Some("64") => return Some(true),
                Some("32") => return Some(false),
                _ => {}
            }
        }
    }
    // Fall back to the exe path (Win64/bin64/x64 vs Win32/x86).
    for exe in windows_executables(json, app_id) {
        let path = exe.to_ascii_lowercase();
        if path.contains("win64") || path.contains("bin64") || path.contains("x64") {
            return Some(true);
        }
        if path.contains("win32") || path.contains("bin32") || path.contains("x86") {
            return Some(false);
        }
    }
    None
}

/// Fetches the Steam install-directory name for `app_id` (the `steamapps/common/<installdir>` folder
/// name), used as the depot download target. Returns `None` when app-info omits it.
pub fn fetch_install_dir(app_id: u32) -> Result<Option<String>, SteamAppInfoError> {
    if app_id == 0 {
        return Err(SteamAppInfoError::InvalidAppId);
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .get(format!("https://api.steamcmd.net/v1/info/{app_id}"))
        .header(USER_AGENT, user_agent())
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAXIMUM_APPINFO_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAXIMUM_APPINFO_BYTES {
        return Err(SteamAppInfoError::TooLarge);
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(install_dir_name(&json, app_id))
}

/// Extracts `config.installdir` from a SteamCMD.net app-info document. Pure, for unit testing.
#[must_use]
pub fn install_dir_name(json: &serde_json::Value, app_id: u32) -> Option<String> {
    let name = json["data"][app_id.to_string()]["config"]["installdir"].as_str()?;
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[derive(Debug, Error)]
pub enum SteamAppInfoError {
    #[error("The selected app has no App ID")]
    InvalidAppId,
    #[error("The Steam app-info response exceeded the maximum allowed size")]
    TooLarge,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_windows_exe_paths_and_skips_other_os() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"3321460":{"config":{"launch":{
                "0":{"executable":"bin64/CrimsonDesert.exe","config":{"oslist":"windows"}},
                "1":{"executable":"CrimsonDesert_Steam.app","config":{"oslist":"macos"}},
                "2":{"executable":"game\\bin\\win64\\extra.exe"}
            }}}}}"#,
        )
        .expect("json");
        let exes = windows_executables(&json, 3_321_460);
        assert_eq!(exes, vec!["bin64/CrimsonDesert.exe", "game/bin/win64/extra.exe"]);
    }

    #[test]
    fn extracts_install_dir() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"730":{"config":{"installdir":"Counter-Strike Global Offensive"}}}}"#,
        )
        .expect("json");
        assert_eq!(
            install_dir_name(&json, 730).as_deref(),
            Some("Counter-Strike Global Offensive")
        );
        let empty: serde_json::Value =
            serde_json::from_str(r#"{"data":{"730":{"config":{}}}}"#).expect("json");
        assert!(install_dir_name(&empty, 730).is_none());
    }

    #[test]
    fn arch_from_osarch_then_path() {
        let osarch: serde_json::Value = serde_json::from_str(
            r#"{"data":{"1":{"config":{"launch":{"0":{"executable":"game.exe","config":{"oslist":"windows","osarch":"64"}}}}}}}"#,
        )
        .expect("json");
        assert_eq!(windows_arch(&osarch, 1), Some(true));

        let by_path: serde_json::Value = serde_json::from_str(
            r#"{"data":{"2":{"config":{"launch":{"0":{"executable":"Binaries/Win64/game.exe","config":{"oslist":"windows"}}}}}}}"#,
        )
        .expect("json");
        assert_eq!(windows_arch(&by_path, 2), Some(true));

        let x86: serde_json::Value = serde_json::from_str(
            r#"{"data":{"3":{"config":{"launch":{"0":{"executable":"bin/win32/game.exe"}}}}}}"#,
        )
        .expect("json");
        assert_eq!(windows_arch(&x86, 3), Some(false));

        let unknown: serde_json::Value =
            serde_json::from_str(r#"{"data":{"4":{"config":{"launch":{"0":{"executable":"game.exe"}}}}}}"#)
                .expect("json");
        assert_eq!(windows_arch(&unknown, 4), None);
    }

    #[test]
    fn missing_launch_yields_empty() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"data":{"730":{"config":{}}}}"#).expect("json");
        assert!(windows_executables(&json, 730).is_empty());
    }
}
