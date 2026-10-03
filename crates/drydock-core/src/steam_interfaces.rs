//! `steam_interfaces.txt` for gbe_fork: which Steamworks interface versions the game's own
//! `steam_api` used.
//!
//! The emu crack replaces the game's `steam_api(64).dll` with gbe_fork's. Newer games name the
//! interface version they want in every call, but older ones call version-less accessors such as
//! `SteamUser()` and rely on the version their original `steam_api` had built in. With that DLL
//! replaced, gbe_fork has to be told — otherwise it hands out its newest version, whose function
//! table the game was not compiled against. gbe_fork's own guide says to always generate the file
//! with its `generate_interfaces` tool; this is that tool, rebuilt so the crack needs no extra
//! download. It reproduces the tool's output line for line: every pattern in the tool's order, every
//! match in the order it occurs in the DLL, and its one special case for `SteamClient`.

use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::emu_template::PeArch;

/// The interface names gbe_fork's `generate_interfaces` looks for, in its order: a fixed prefix and
/// whether the version digits that must follow it (`\d+`) are part of the pattern.
const INTERFACE_PATTERNS: &[(&str, bool)] = &[
    ("STEAMAPPS_INTERFACE_VERSION", true),
    ("SteamApps", true),
    ("STEAMAPPLIST_INTERFACE_VERSION", true),
    ("STEAMAPPTICKET_INTERFACE_VERSION", true),
    ("SteamClient", true),
    ("STEAMCONTROLLER_INTERFACE_VERSION", false),
    ("SteamController", true),
    ("SteamFriends", true),
    ("SteamGameServerStats", true),
    ("SteamGameCoordinator", true),
    ("SteamGameServer", true),
    ("STEAMHTMLSURFACE_INTERFACE_VERSION_", true),
    ("STEAMHTTP_INTERFACE_VERSION", true),
    ("SteamInput", true),
    ("STEAMINVENTORY_INTERFACE_V", true),
    ("SteamMatchMakingServers", true),
    ("SteamMatchMaking", true),
    ("SteamMatchGameSearch", true),
    ("SteamParties", true),
    ("STEAMMUSIC_INTERFACE_VERSION", true),
    ("STEAMMUSICREMOTE_INTERFACE_VERSION", true),
    ("SteamNetworkingMessages", true),
    ("SteamNetworkingSockets", true),
    ("SteamNetworkingUtils", true),
    ("SteamNetworking", true),
    ("STEAMPARENTALSETTINGS_INTERFACE_VERSION", true),
    ("STEAMREMOTEPLAY_INTERFACE_VERSION", true),
    ("STEAMREMOTESTORAGE_INTERFACE_VERSION", true),
    ("STEAMSCREENSHOTS_INTERFACE_VERSION", true),
    ("STEAMTIMELINE_INTERFACE_V", true),
    ("STEAMUGC_INTERFACE_VERSION", true),
    ("SteamUser", true),
    ("STEAMUSERSTATS_INTERFACE_VERSION", true),
    ("SteamUtils", true),
    ("STEAMVIDEO_INTERFACE_V", true),
    ("STEAMUNIFIEDMESSAGES_INTERFACE_VERSION", true),
    ("SteamMasterServerUpdater", true),
];

/// Strings only an emulator's `steam_api` carries (gbe_fork's and Goldberg's read `steam_settings\`
/// and `configs.user.ini`, older emus a `steam_emu.ini`). Such a DLL is never the game's original:
/// it lists the interfaces *it* supports, not the ones the game was built against.
const EMULATOR_MARKERS: &[&[u8]] = &[b"steam_settings", b"configs.user.ini", b"steam_emu.ini"];

/// The non-overlapping matches of one pattern, left to right — what `std::regex` iterating
/// `prefix\d+` (or the bare prefix) over the DLL yields.
fn matches(contents: &[u8], prefix: &str, digits: bool) -> Vec<String> {
    let prefix = prefix.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at + prefix.len() <= contents.len() {
        let Some(offset) = contents[at..]
            .windows(prefix.len())
            .position(|window| window == prefix)
        else {
            break;
        };
        let start = at + offset;
        let mut end = start + prefix.len();
        if digits {
            while end < contents.len() && contents[end].is_ascii_digit() {
                end += 1;
            }
            if end == start + prefix.len() {
                // The prefix without a version after it is no match; look on from the next byte.
                at = start + 1;
                continue;
            }
        }
        found.push(String::from_utf8_lossy(&contents[start..end]).into_owned());
        at = end;
    }
    found
}

/// The `steam_interfaces.txt` for a game whose original `steam_api` is `contents`, or `None` when
/// it names no interface at all (the tool fails there too, and an empty file would change nothing).
#[must_use]
pub fn steam_interfaces(contents: &[u8]) -> Option<String> {
    let mut lines = Vec::new();
    for (prefix, digits) in INTERFACE_PATTERNS {
        let mut found = matches(contents, prefix, *digits);
        // The tool's special case: newer SDKs keep `SteamClient()` as their one legacy export, and it
        // still returns SteamClient017, so when that is among several versions it is the one kept.
        if *prefix == "SteamClient" && found.len() > 1 && found.iter().any(|name| name == "SteamClient017") {
            found.retain(|name| name == "SteamClient017");
        }
        lines.extend(found);
    }
    if lines.is_empty() {
        return None;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    Some(text)
}

/// Whether `contents` is an emulator's `steam_api` rather than Valve's.
#[must_use]
pub fn is_emulator_steam_api(contents: &[u8]) -> bool {
    EMULATOR_MARKERS
        .iter()
        .any(|marker| contents.windows(marker.len()).any(|window| window == *marker))
}

/// The game's own `steam_api(64).dll` for `arch`, read before the crack replaces it.
///
/// Looked for next to the executable first (where the crack puts gbe_fork's), then as the `.bak` an
/// earlier crack left there, then anywhere in the game folder — Unreal games keep it under
/// `Engine\Binaries\ThirdParty\Steamworks\…`. An emulator's DLL is passed over wherever it is, so a
/// game cracked before still yields its original. Returns the path and its contents.
#[must_use]
pub fn original_steam_api(game_root: &Path, exe_dir: &Path, arch: PeArch) -> Option<(PathBuf, Vec<u8>)> {
    let name = match arch {
        PeArch::X64 => "steam_api64.dll",
        PeArch::X86 => "steam_api.dll",
    };
    let backup = format!("{name}.bak");
    let original = |path: &Path| -> Option<(PathBuf, Vec<u8>)> {
        let contents = fs::read(path).ok()?;
        (!contents.is_empty() && !is_emulator_steam_api(&contents)).then(|| (path.to_path_buf(), contents))
    };
    if let Some(found) = original(&exe_dir.join(name)).or_else(|| original(&exe_dir.join(&backup))) {
        return Some(found);
    }
    // Shallowest first, then by name, so the choice is the same every time. Bounded, because a game
    // folder can hold hundreds of thousands of files and this is only a best effort.
    let mut candidates: Vec<(usize, PathBuf)> = WalkDir::new(game_root)
        .max_depth(12)
        .into_iter()
        .filter_map(Result::ok)
        .take(400_000)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            let file = entry.file_name().to_string_lossy();
            file.eq_ignore_ascii_case(name) || file.eq_ignore_ascii_case(&backup)
        })
        .map(|entry| (entry.depth(), entry.into_path()))
        .collect();
    candidates.sort();
    candidates.into_iter().find_map(|(_, path)| original(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a Valve `steam_api64.dll`: the interface names sit as C strings among binary
    /// noise, the way the real DLL's `.rdata` holds them.
    fn dll(names: &[&str]) -> Vec<u8> {
        let mut bytes = b"MZ\x90\x00\x03\x00binary".to_vec();
        for name in names {
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(b"\x00\x7f\x10");
        }
        bytes
    }

    #[test]
    fn interfaces_come_out_in_the_tools_order_with_their_versions() {
        let contents = dll(&[
            "SteamUser019",
            "SteamClient017",
            "SteamFriends015",
            "STEAMAPPS_INTERFACE_VERSION008",
            "SteamUserStats", // no version: not an interface name
            "SteamGameServerStats001",
            "SteamGameServer013",
            "SteamNetworkingSockets012",
            "SteamNetworking006",
            "STEAMHTMLSURFACE_INTERFACE_VERSION_005",
            "STEAMCONTROLLER_INTERFACE_VERSION",
        ]);
        assert_eq!(
            steam_interfaces(&contents).as_deref(),
            Some(
                "STEAMAPPS_INTERFACE_VERSION008\nSteamClient017\nSTEAMCONTROLLER_INTERFACE_VERSION\n\
                 SteamFriends015\nSteamGameServerStats001\nSteamGameServer013\n\
                 STEAMHTMLSURFACE_INTERFACE_VERSION_005\nSteamNetworkingSockets012\nSteamNetworking006\n\
                 SteamUser019\n"
            )
        );
    }

    #[test]
    fn several_client_versions_with_017_among_them_keep_only_017() {
        let both = steam_interfaces(&dll(&["SteamClient021", "SteamClient017", "SteamUser023"])).unwrap();
        assert_eq!(both, "SteamClient017\nSteamUser023\n");
        // Without 017, every version found stays, as the tool leaves them.
        let other = steam_interfaces(&dll(&["SteamClient020", "SteamClient021"])).unwrap();
        assert_eq!(other, "SteamClient020\nSteamClient021\n");
    }

    #[test]
    fn a_dll_without_interface_names_gives_no_file() {
        assert_eq!(steam_interfaces(b"MZ just code, no names"), None);
        assert_eq!(steam_interfaces(&dll(&["SteamUser", "SteamClient"])), None);
    }

    #[test]
    fn an_emulators_steam_api_is_never_taken_for_the_original() {
        let gbe = [
            dll(&["SteamClient017", "SteamUser023"]),
            b"steam_settings\\configs.user.ini".to_vec(),
        ]
        .concat();
        assert!(is_emulator_steam_api(&gbe));
        assert!(!is_emulator_steam_api(&dll(&["SteamUser019"])));

        let game = tempfile::tempdir().unwrap();
        let exe_dir = game.path().join("Binaries").join("Win64");
        fs::create_dir_all(&exe_dir).unwrap();
        // Cracked before: gbe_fork's DLL next to the exe, the original kept as .bak beside it.
        fs::write(exe_dir.join("steam_api64.dll"), &gbe).unwrap();
        fs::write(exe_dir.join("steam_api64.dll.bak"), dll(&["SteamUser019"])).unwrap();
        let (path, contents) = original_steam_api(game.path(), &exe_dir, PeArch::X64).unwrap();
        assert_eq!(path, exe_dir.join("steam_api64.dll.bak"));
        assert_eq!(steam_interfaces(&contents).unwrap(), "SteamUser019\n");
    }

    #[test]
    fn the_original_is_found_where_unreal_keeps_it() {
        let game = tempfile::tempdir().unwrap();
        let exe_dir = game.path().join("Game").join("Binaries").join("Win64");
        let steamworks = game
            .path()
            .join("Engine/Binaries/ThirdParty/Steamworks/Steamv153/Win64");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(&steamworks).unwrap();
        fs::write(steamworks.join("steam_api64.dll"), dll(&["SteamUser021"])).unwrap();
        // The 32-bit DLL is not what an x64 crack needs.
        fs::write(steamworks.join("steam_api.dll"), dll(&["SteamUser009"])).unwrap();
        let (path, _) = original_steam_api(game.path(), &exe_dir, PeArch::X64).unwrap();
        assert_eq!(path, steamworks.join("steam_api64.dll"));
        assert!(original_steam_api(game.path(), &exe_dir, PeArch::X86).is_some());
        assert!(original_steam_api(&exe_dir, &exe_dir, PeArch::X64).is_none());
    }
}
