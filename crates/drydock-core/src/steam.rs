use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use sysinfo::{ProcessesToUpdate, System};

#[cfg(windows)]
use winreg::RegKey;
#[cfg(windows)]
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::models::SteamManifest;

const STEAMWORKS_COMMON_REDISTRIBUTABLES: u32 = 228_980;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SteamDiscovery {
    pub root: Option<PathBuf>,
    pub libraries: Vec<PathBuf>,
}

/// Returns true when `path` looks like a real Steam installation.
///
/// This mirrors the C# `SteamLocator.IsValidSteamDirectory`: on Windows the folder
/// must contain `steam.exe`. On other platforms the platform-specific launcher is
/// accepted so the same checks compile and can be exercised in tests.
#[must_use]
pub fn is_valid_steam_directory(path: Option<&Path>) -> bool {
    let Some(path) = path.filter(|path| !path.as_os_str().is_empty()) else {
        return false;
    };
    #[cfg(windows)]
    {
        path.join("steam.exe").is_file()
    }
    #[cfg(not(windows))]
    {
        path.join("steam.exe").is_file()
            || path.join("steam.sh").is_file()
            || path.join("steam_osx").is_file()
    }
}

pub fn discover_steam(configured_root: Option<&Path>) -> SteamDiscovery {
    let mut roots = Vec::new();
    if let Some(path) = configured_root.filter(|path| !path.as_os_str().is_empty()) {
        roots.push(path.to_path_buf());
    }
    roots.extend(running_steam_roots());
    #[cfg(windows)]
    roots.extend(registry_steam_roots());
    roots.extend(common_steam_roots());

    let root = roots.into_iter().find(|candidate| is_steam_root(candidate));
    let Some(root) = root else {
        return SteamDiscovery::default();
    };

    let mut libraries = BTreeSet::from([root.clone()]);
    let library_file = root.join("steamapps").join("libraryfolders.vdf");
    if let Ok(text) = fs::read_to_string(library_file) {
        for library in parse_library_paths(&text) {
            if library.join("steamapps").is_dir() {
                libraries.insert(library);
            }
        }
    }

    SteamDiscovery {
        root: Some(root),
        libraries: libraries.into_iter().collect(),
    }
}

pub fn load_manifests(discovery: &SteamDiscovery) -> Result<Vec<SteamManifest>, SteamError> {
    let mut manifests = BTreeMap::<u32, SteamManifest>::new();
    for library in &discovery.libraries {
        let steamapps = library.join("steamapps");
        let entries = match fs::read_dir(&steamapps) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SteamError::ReadDirectory {
                    path: steamapps,
                    source,
                });
            }
        };

        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(manifest) = parse_manifest(&text, library, &path)
                && manifest.app_id != STEAMWORKS_COMMON_REDISTRIBUTABLES
            {
                match manifests.entry(manifest.app_id) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(manifest);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry)
                        if manifest.is_fully_installed() && !entry.get().is_fully_installed() =>
                    {
                        entry.insert(manifest);
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {}
                }
            }
        }
    }

    let mut manifests: Vec<_> = manifests.into_values().collect();
    manifests.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.app_id.cmp(&right.app_id))
    });
    Ok(manifests)
}

pub fn parse_manifest(
    text: &str,
    library_path: &Path,
    manifest_path: &Path,
) -> Result<SteamManifest, SteamError> {
    let values = quoted_pairs(text);
    let app_id = required(&values, "appid")?
        .parse::<u32>()
        .map_err(|_| SteamError::InvalidField("appid"))?;
    let name = required(&values, "name")?.trim().to_owned();
    let install_dir_name = required(&values, "installdir")?.trim().to_owned();
    if name.is_empty() || install_dir_name.is_empty() {
        return Err(SteamError::InvalidField("name or installdir"));
    }
    let components = install_dir_name.split(['/', '\\']).collect::<Vec<_>>();
    if components.len() != 1 || matches!(components[0], "" | "." | "..") || components[0].contains(':') {
        return Err(SteamError::InvalidField("installdir"));
    }

    Ok(SteamManifest {
        app_id,
        name,
        install_dir_name,
        library_path: library_path.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        state_flags: number_or_zero(&values, "StateFlags"),
        bytes_to_download: number_or_zero(&values, "BytesToDownload"),
        bytes_downloaded: number_or_zero(&values, "BytesDownloaded"),
        bytes_to_stage: number_or_zero(&values, "BytesToStage"),
        bytes_staged: number_or_zero(&values, "BytesStaged"),
        size_on_disk: optional_number(&values, "SizeOnDisk"),
    })
}

fn required<'a>(values: &'a HashMap<String, String>, name: &'static str) -> Result<&'a str, SteamError> {
    values
        .get(&name.to_ascii_lowercase())
        .map(String::as_str)
        .ok_or(SteamError::MissingField(name))
}

fn number_or_zero(values: &HashMap<String, String>, name: &str) -> u64 {
    values
        .get(&name.to_ascii_lowercase())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn optional_number(values: &HashMap<String, String>, name: &str) -> Option<u64> {
    values
        .get(&name.to_ascii_lowercase())
        .and_then(|value| value.parse().ok())
}

fn quoted_pairs(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let strings = quoted_strings(line);
        if strings.len() >= 2 {
            values.insert(strings[0].to_ascii_lowercase(), strings[1].clone());
        }
    }
    values
}

fn parse_library_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let values = quoted_strings(line);
            (values.len() >= 2 && values[0].eq_ignore_ascii_case("path")).then(|| PathBuf::from(&values[1]))
        })
        .collect()
}

fn quoted_strings(line: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in line.chars() {
        if !quoted {
            if character == '"' {
                quoted = true;
                current.clear();
            }
            continue;
        }
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            output.push(current.clone());
            quoted = false;
        } else {
            current.push(character);
        }
    }
    output
}

fn common_steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    {
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(value) = env::var_os(variable) {
                roots.push(PathBuf::from(value).join("Steam"));
            }
        }
    }

    #[cfg(target_os = "linux")]
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".steam").join("steam"));
        roots.push(home.join(".steam").join("root"));
        roots.push(home.join(".steam").join("debian-installation"));
        roots.push(home.join(".local").join("share").join("Steam"));
        roots.push(
            home.join(".var")
                .join("app")
                .join("com.valvesoftware.Steam")
                .join(".local")
                .join("share")
                .join("Steam"),
        );
        roots.push(
            home.join("snap")
                .join("steam")
                .join("common")
                .join(".local")
                .join("share")
                .join("Steam"),
        );
    }
    #[cfg(target_os = "linux")]
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home).join("Steam"));
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = env::var_os("HOME") {
        roots.push(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Steam"),
        );
    }

    roots
}

fn running_steam_roots() -> Vec<PathBuf> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .filter(|process| {
            matches!(
                process.name().to_string_lossy().to_ascii_lowercase().as_str(),
                "steam.exe" | "steam" | "steam.sh" | "steam_osx"
            )
        })
        .filter_map(|process| process.exe().and_then(Path::parent).map(Path::to_path_buf))
        .collect()
}

#[cfg(windows)]
fn registry_steam_roots() -> Vec<PathBuf> {
    let candidates = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    candidates
        .into_iter()
        .filter_map(|(hive, key, value)| {
            RegKey::predef(hive)
                .open_subkey(key)
                .ok()
                .and_then(|key| key.get_value::<String, _>(value).ok())
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
        })
        .collect()
}

fn is_steam_root(path: &Path) -> bool {
    if !path.join("steamapps").is_dir() {
        return false;
    }
    #[cfg(target_os = "windows")]
    return path.join("steam.exe").is_file();
    #[cfg(target_os = "linux")]
    return path.join("steam.sh").is_file() || path.join("ubuntu12_32").join("steam").is_file();
    #[cfg(target_os = "macos")]
    return path.join("Steam.AppBundle").exists() || path.join("steamapps").is_dir();
    #[allow(unreachable_code)]
    false
}

#[derive(Debug, Error)]
pub enum SteamError {
    #[error("cannot read Steam directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read Steam manifest {path}: {source}")]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Steam manifest is missing {0}")]
    MissingField(&'static str),
    #[error("Steam manifest contains an invalid {0}")]
    InvalidField(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installed_manifest() {
        let root = tempfile::tempdir().expect("tempdir");
        let install = root
            .path()
            .join("steamapps")
            .join("common")
            .join("Persona 4 Golden");
        fs::create_dir_all(&install).expect("create install dir");
        fs::write(install.join("game.exe"), b"game").expect("write game file");
        let path = root.path().join("steamapps").join("appmanifest_111300.acf");
        let text = r#"
            "AppState"
            {
                "appid" "111300"
                "name" "Persona 4 Golden"
                "StateFlags" "4"
                "installdir" "Persona 4 Golden"
                "BytesToDownload" "42"
                "BytesDownloaded" "42"
            }
        "#;
        let manifest = parse_manifest(text, root.path(), &path).expect("parse manifest");
        assert_eq!(manifest.app_id, 111_300);
        assert_eq!(manifest.name, "Persona 4 Golden");
        assert!(manifest.is_fully_installed());

        let pending = root.path().join("steamapps").join("downloading").join("111300");
        fs::create_dir_all(&pending).expect("create pending dir");
        fs::write(pending.join("chunk"), b"pending").expect("write pending file");
        assert!(!manifest.is_fully_installed());
    }

    #[test]
    fn rejects_install_directory_traversal() {
        let text = r#"
            "AppState"
            {
                "appid" "42"
                "name" "Unsafe"
                "StateFlags" "4"
                "installdir" "..\\outside"
            }
        "#;
        assert!(matches!(
            parse_manifest(text, Path::new("C:/Steam"), Path::new("appmanifest_42.acf")),
            Err(SteamError::InvalidField("installdir"))
        ));
    }

    #[test]
    fn rejects_drive_relative_install_directory() {
        // A drive-relative prefix ("C:evil") would make Path::join escape the library folder
        // on Windows, so ":" must be rejected alongside separators and dot entries.
        for value in ["C:evil", "C:", "folder:stream"] {
            let text = format!(
                r#"
                    "AppState"
                    {{
                        "appid" "42"
                        "name" "Unsafe"
                        "StateFlags" "4"
                        "installdir" "{value}"
                    }}
                "#
            );
            assert!(
                matches!(
                    parse_manifest(&text, Path::new("C:/Steam"), Path::new("appmanifest_42.acf")),
                    Err(SteamError::InvalidField("installdir"))
                ),
                "expected {value:?} to be rejected"
            );
        }
    }

    #[test]
    fn parses_library_paths_and_unescapes_windows_separator() {
        let paths = parse_library_paths(r#""path" "D:\\SteamLibrary""#);
        assert_eq!(paths, vec![PathBuf::from(r"D:\SteamLibrary")]);
    }

    #[test]
    fn manifest_scan_ignores_system_and_malformed_apps_and_prefers_complete_duplicate() {
        let first = tempfile::tempdir().expect("first library");
        let second = tempfile::tempdir().expect("second library");
        for library in [first.path(), second.path()] {
            fs::create_dir_all(library.join("steamapps").join("common")).expect("steamapps");
        }
        let incomplete = r#"
            "AppState"
            {
                "appid" "42"
                "name" "Incomplete Copy"
                "StateFlags" "4"
                "installdir" "Game"
                "BytesToDownload" "100"
                "BytesDownloaded" "50"
            }
        "#;
        let complete = r#"
            "AppState"
            {
                "appid" "42"
                "name" "Complete Copy"
                "StateFlags" "4"
                "installdir" "Game"
                "BytesToDownload" "100"
                "BytesDownloaded" "100"
            }
        "#;
        let redistributable = r#"
            "AppState"
            {
                "appid" "228980"
                "name" "Steamworks Common Redistributables"
                "StateFlags" "4"
                "installdir" "Steamworks Shared"
            }
        "#;
        fs::write(first.path().join("steamapps/appmanifest_42.acf"), incomplete).expect("manifest");
        fs::write(
            first.path().join("steamapps/appmanifest_broken.acf"),
            "not a manifest",
        )
        .expect("broken");
        fs::write(
            first.path().join("steamapps/appmanifest_228980.acf"),
            redistributable,
        )
        .expect("redistributable");
        fs::write(second.path().join("steamapps/appmanifest_42.acf"), complete).expect("manifest");
        fs::create_dir_all(second.path().join("steamapps/common/Game")).expect("game dir");
        fs::write(second.path().join("steamapps/common/Game/game.bin"), b"game").expect("game");

        let manifests = load_manifests(&SteamDiscovery {
            root: Some(first.path().to_path_buf()),
            libraries: vec![first.path().to_path_buf(), second.path().to_path_buf()],
        })
        .expect("scan");

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].app_id, 42);
        assert_eq!(manifests[0].name, "Complete Copy");
        assert!(manifests[0].is_fully_installed());
    }
}
