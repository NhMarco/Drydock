//! Fetches a Steam app's Windows launch executable paths from public `app_info`.
//!
//! Used to verify a foreign (non-Steam) game install: the launch executable's *relative* path
//! (e.g. `bin64/CrimsonDesert.exe`) is expressed relative to the game's install root — the same
//! root the activation payload zip extracts into — so locating that executable in a chosen folder
//! pins the exact extraction root. Source: the public SteamCMD.net mirror of Steam `app_info`
//! (`config.launch`), which needs no API key, so the client fetches it directly.

use std::collections::BTreeSet;
use std::io::Read;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::USER_AGENT;
use thiserror::Error;

use crate::version::user_agent;

const MAXIMUM_APPINFO_BYTES: u64 = 8 * 1024 * 1024;

/// Fetches an app's public `app_info` document from the SteamCMD.net mirror.
fn fetch_app_info(app_id: u32) -> Result<serde_json::Value, SteamAppInfoError> {
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
    Ok(serde_json::from_slice(&bytes)?)
}

/// The depots of `app_id` that Steam marks for another operating system.
///
/// A provider's package carries **every** depot an app has — the Windows, macOS and Linux builds of
/// the same game, each the game's full size. Downloading all of them takes several times the disk
/// space Steam would use, and a verify would report every foreign-OS file as missing. Only depots
/// app-info explicitly assigns to another OS are named here: anything it does not mention stays,
/// because the package knows about depots app-info does not (content shared from another app, say).
pub fn fetch_non_windows_depots(app_id: u32) -> Result<BTreeSet<u32>, SteamAppInfoError> {
    Ok(non_windows_depots(&fetch_app_info(app_id)?, app_id))
}

/// Reads the depot IDs assigned to another OS out of a SteamCMD.net app-info document: a depot whose
/// `config.oslist` names an OS but not Windows. Pure, for unit testing.
#[must_use]
pub fn non_windows_depots(json: &serde_json::Value, app_id: u32) -> BTreeSet<u32> {
    let Some(depots) = json["data"][app_id.to_string()]["depots"].as_object() else {
        return BTreeSet::new();
    };
    let mut foreign = BTreeSet::new();
    for (key, depot) in depots {
        // `branches` and the other non-depot keys sit in the same object.
        let Ok(depot_id) = key.parse::<u32>() else {
            continue;
        };
        let oslist = depot["config"]["oslist"].as_str().unwrap_or_default();
        if !oslist.is_empty() && !oslist.to_ascii_lowercase().contains("windows") {
            foreign.insert(depot_id);
        }
    }
    foreign
}

/// Returns the distinct Windows launch executables for `app_id`, as forward-slash relative paths
/// (e.g. `bin64/CrimsonDesert.exe`). Non-Windows and non-`.exe` launch entries are dropped.
pub fn fetch_windows_executables(app_id: u32) -> Result<Vec<String>, SteamAppInfoError> {
    if app_id == 0 {
        return Err(SteamAppInfoError::InvalidAppId);
    }
    let json = fetch_app_info(app_id)?;
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
        if crate::safe_path::relative_path(&normalized).is_none() {
            continue;
        }
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
    let json = fetch_app_info(app_id)?;
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
    let json = fetch_app_info(app_id)?;
    Ok(install_dir_name(&json, app_id))
}

/// Extracts `config.installdir` from a SteamCMD.net app-info document. Pure, for unit testing.
#[must_use]
pub fn install_dir_name(json: &serde_json::Value, app_id: u32) -> Option<String> {
    let name = json["data"][app_id.to_string()]["config"]["installdir"].as_str()?;
    let trimmed = name.trim();
    crate::safe_path::is_portable_path_segment(trimmed).then(|| trimmed.to_owned())
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
    fn only_depots_of_another_os_are_named() {
        // Shaped like Baldur's Gate 3: the same game as a Windows, a macOS and a Linux depot, plus
        // language and DLC depots, and the `branches` key that sits beside them.
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"1086940":{"depots":{
                "1086941":{"config":{"oslist":"windows"}},
                "1086944":{"config":{"oslist":"windows","language":"german"}},
                "1419660":{"config":{"oslist":"macos"}},
                "2378501":{"config":{"oslist":"linux"}},
                "2330358":{"config":{"oslist":"windows"},"dlcappid":"2956320"},
                "2378500":{"dlcappid":"2378500"},
                "228990":{"config":{"oslist":"windows"},"depotfromapp":"228980"},
                "branches":{"public":{"buildid":"1"}}
            }}}}"#,
        )
        .expect("json");
        assert_eq!(
            non_windows_depots(&json, 1_086_940),
            BTreeSet::from([1_419_660, 2_378_501]),
            "the Windows, language, DLC and shared-redistributable depots all stay"
        );
        // Nothing to go by: the caller keeps the package as it is.
        assert!(non_windows_depots(&json, 730).is_empty());
        let empty: serde_json::Value =
            serde_json::from_str(r#"{"data":{"730":{"depots":{"branches":{}}}}}"#).expect("json");
        assert!(non_windows_depots(&empty, 730).is_empty());
    }

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
