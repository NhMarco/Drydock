use std::collections::BTreeSet;
use std::path::Path;

use sysinfo::{ProcessesToUpdate, System};
use walkdir::WalkDir;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConflictingSoftwareStatus {
    pub names: Vec<String>,
}

impl ConflictingSoftwareStatus {
    #[must_use]
    pub fn detected(&self) -> bool {
        !self.names.is_empty()
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self.names.as_slice() {
            [] => String::new(),
            [name] => format!(
                "{name} was found. It manages the same Steam files and can make activation fail. Remove it, then restart Steam."
            ),
            names => format!(
                "{} were found. They manage the same Steam files and can make activation fail. Remove them, then restart Steam.",
                names.join(" and ")
            ),
        }
    }
}

pub fn detect_conflicting_software(steam_directory: Option<&Path>) -> ConflictingSoftwareStatus {
    let processes = running_processes();
    let installed = installed_program_names();
    let mut names = Vec::new();
    if processes
        .iter()
        .any(|name| matches_name(name, &["steamtools", "steamtools_launcher"]))
        || installed
            .iter()
            .any(|name| contains_any(name, &["steamtools", "steam tools"]))
        || common_folder_has_program_files(&["Steamtools", "SteamTools"])
    {
        names.push("SteamTools".to_owned());
    }
    if processes
        .iter()
        .any(|name| matches_name(name, &["dllinjector", "greenluma"]))
        || installed.iter().any(|name| contains_any(name, &["greenluma"]))
        || steam_directory.is_some_and(has_greenluma_artifact)
    {
        names.push("GreenLuma".to_owned());
    }
    if processes
        .iter()
        .any(|name| matches_name(name, &["tokeer", "tokeerdrm"]))
        || installed.iter().any(|name| contains_any(name, &["tokeer"]))
        || common_folder_has_program_files(&["TokeerDRM", "Tokeer"])
    {
        names.push("Tokeer DRM".to_owned());
    }
    // A DRM manipulator that patched Steam's own binaries typically leaves a `.bak` of the
    // original next to it. Our Steam Service does write payload files into the Steam root, but
    // it backs up any replaced file into a temp dir (never a `.bak` beside it), so these
    // `.bak` artifacts remain a reliable third-party tamper signal.
    if steam_directory.is_some_and(has_steam_tamper_artifacts) {
        names.push("Modified Steam files".to_owned());
    }
    ConflictingSoftwareStatus { names }
}

/// Backups of Steam's core binaries in the Steam root — left behind when a third-party tool
/// patches Steam in place. Deliberately narrow to keep false positives near zero.
fn has_steam_tamper_artifacts(steam: &Path) -> bool {
    const SUSPICIOUS_BACKUPS: &[&str] = &[
        "steam.exe.bak",
        "steam.dll.bak",
        "steamclient.dll.bak",
        "steamclient64.dll.bak",
        "steamservice.dll.bak",
        "tier0_s.dll.bak",
        "tier0_s64.dll.bak",
        "vstdlib_s.dll.bak",
        "vstdlib_s64.dll.bak",
    ];
    SUSPICIOUS_BACKUPS.iter().any(|name| steam.join(name).is_file())
}

fn running_processes() -> BTreeSet<String> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .filter_map(|process| process.name().to_str())
        .map(normalized_name)
        .collect()
}

fn matches_name(value: &str, expected: &[&str]) -> bool {
    let value = normalized_name(value);
    expected.iter().any(|expected| value == *expected)
}

fn contains_any(value: &str, expected: &[&str]) -> bool {
    let value = value.to_ascii_lowercase();
    expected.iter().any(|expected| value.contains(expected))
}

fn normalized_name(value: &str) -> String {
    value.trim().trim_end_matches(".exe").to_ascii_lowercase()
}

fn has_greenluma_artifact(steam: &Path) -> bool {
    // The year-specific DLL names (e.g. GreenLuma_2024_x86.dll) change annually; match the
    // prefix instead so new releases are detected without code updates.
    has_greenluma_dll(steam)
        || ["DLLInjector.exe", "DLLInjector.ini"]
            .iter()
            .any(|name| steam.join(name).is_file())
        || directory_has_program_files(&steam.join("AppList"))
}

fn has_greenluma_dll(steam: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(steam) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("greenluma_")
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
    })
}

fn common_folder_has_program_files(folder_names: &[&str]) -> bool {
    common_install_roots().iter().any(|root| {
        folder_names
            .iter()
            .any(|folder| directory_has_program_files(&root.join(folder)))
    })
}

fn directory_has_program_files(directory: &Path) -> bool {
    if !directory.is_dir() {
        return false;
    }
    WalkDir::new(directory)
        .follow_links(false)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .any(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("dll")
                    })
        })
}

#[cfg(windows)]
fn common_install_roots() -> Vec<std::path::PathBuf> {
    ["LOCALAPPDATA", "APPDATA", "ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(Into::into)
        .collect()
}

#[cfg(not(windows))]
fn common_install_roots() -> Vec<std::path::PathBuf> {
    Vec::new()
}

#[cfg(windows)]
fn installed_program_names() -> BTreeSet<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let mut names = BTreeSet::new();
    let paths = [
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
    ];
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for path in paths {
            let Ok(key) = RegKey::predef(root).open_subkey(path) else {
                continue;
            };
            for child in key.enum_keys().flatten() {
                let Ok(entry) = key.open_subkey(child) else {
                    continue;
                };
                if let Ok(name) = entry.get_value::<String, _>("DisplayName")
                    && !name.trim().is_empty()
                {
                    names.insert(name);
                }
            }
        }
    }
    names
}

#[cfg(not(windows))]
fn installed_program_names() -> BTreeSet<String> {
    BTreeSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_artifact_detection_is_specific_and_ignores_empty_folders() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir(root.path().join("AppList")).expect("app list");
        assert!(!has_greenluma_artifact(root.path()));
        fs::write(root.path().join("AppList").join("entry.dll"), b"test").expect("artifact");
        assert!(has_greenluma_artifact(root.path()));
    }

    #[test]
    fn steam_tamper_detection_flags_only_binary_backups() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(!has_steam_tamper_artifacts(root.path()));
        // A save-game or config backup elsewhere must not trip it.
        fs::write(root.path().join("userdata.bak"), b"x").expect("unrelated");
        assert!(!has_steam_tamper_artifacts(root.path()));
        // A backup of a patched Steam binary is a real tamper signal.
        fs::write(root.path().join("steamclient64.dll.bak"), b"MZ").expect("backup");
        assert!(has_steam_tamper_artifacts(root.path()));
    }

    #[test]
    fn messages_are_actionable() {
        let status = ConflictingSoftwareStatus {
            names: vec!["GreenLuma".into()],
        };
        assert!(status.detected());
        assert!(status.message().contains("restart Steam"));
    }

    use std::fs;
}
