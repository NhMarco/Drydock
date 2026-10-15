//! Which `.exe` is the game itself — the one the emu crack has to sit next to.
//!
//! Steam's launch list names what Steam starts, and that is not always the game. An Unreal Engine game
//! ships a small starter at the top of its folder (`Permafrost.exe`) that does nothing but start the
//! real game, `Permafrost/Binaries/Win64/Permafrost-Win64-Shipping.exe`, as a second process. The
//! loader DLLs only take effect in the process that loads them, so a crack placed beside the starter
//! never reaches the game. This picks the game's exe from what is in the folder, and says why, so the
//! user can confirm the choice or pick another before any file is placed.

use std::path::Path;

use walkdir::WalkDir;

/// How deep to look for executables in a game folder (an Unreal game's sits four folders down).
const MAX_DEPTH: usize = 8;
/// Executables to look at, at most: a game folder has a handful, and the walk stays bounded anyway.
const MAX_EXES: usize = 2_000;

/// Why an exe was taken as the game's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExeReason {
    /// The user chose it (now or earlier).
    Picked,
    /// Steam starts it, and it is the game.
    SteamListed,
    /// Steam starts `launcher`, an Unreal Engine starter; this is the game it starts.
    UnrealShipping { launcher: String },
    /// Steam names none that is here, and this is the only game exe in the folder.
    OnlyExe,
}

/// The game's exe, relative to the game folder with forward slashes (`Bin/Win64/Game.exe`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameExe {
    pub relative: String,
    pub reason: ExeReason,
}

/// Every `.exe` under `root`, relative and with forward slashes, in a stable order.
#[must_use]
pub fn exes_under(root: &Path) -> Vec<String> {
    let mut exes: Vec<String> = WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            Path::new(entry.file_name())
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        })
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .take(MAX_EXES)
        .collect();
    exes.sort();
    exes
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn stem(path: &str) -> &str {
    let name = file_name(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// Helpers and installers that sit in game folders but are never the game.
fn is_helper(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    let lowered = path.to_ascii_lowercase();
    [
        "crashreport",
        "cefsubprocess",
        "unins",
        "setup",
        "redist",
        "vcredist",
        "dxsetup",
        "dotnet",
        "uninstall",
        "easyanticheat",
        "battleye",
        "be_service",
    ]
    .iter()
    .any(|marker| name.contains(marker))
        || lowered.contains("/_commonredist/")
        || lowered.starts_with("_commonredist/")
        || lowered.contains("/redist/")
}

/// An Unreal Engine shipping build of the game (`…/Binaries/Win64/Name-Win64-Shipping.exe`), not a
/// dedicated server's.
fn is_shipping(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    let name = file_name(&lowered);
    name.ends_with("-shipping.exe")
        && lowered.contains("/binaries/")
        && !name.contains("server")
        && !lowered.starts_with("engine/")
        && !is_helper(path)
}

/// The shipping exe an Unreal starter at `launcher` starts, if `launcher` is one: a shipping build
/// below the starter's own folder, preferably the one named after it.
fn unreal_game_for(launcher: &str, exes: &[String]) -> Option<String> {
    if is_shipping(launcher) {
        return None;
    }
    let base = parent(launcher);
    let below = |path: &&String| base.is_empty() || path.starts_with(&format!("{base}/"));
    let shipping: Vec<&String> = exes
        .iter()
        .filter(|path| is_shipping(path))
        .filter(below)
        .collect();
    let launcher_stem = stem(launcher).to_ascii_lowercase();
    shipping
        .iter()
        .find(|path| {
            stem(path)
                .to_ascii_lowercase()
                .starts_with(&format!("{launcher_stem}-"))
        })
        .or_else(|| (shipping.len() == 1).then(|| &shipping[0]))
        .map(|path| (*path).clone())
}

/// The game's exe among `exes` (what is in the folder), given what Steam launches (`listed`) and what
/// the user chose for the game before (`picked`). `None` when nothing can be told — then the user has
/// to choose.
#[must_use]
pub fn find_game_exe(exes: &[String], listed: &[String], picked: Option<&str>) -> Option<GameExe> {
    let present = |wanted: &str| {
        let wanted = wanted.replace('\\', "/");
        exes.iter().find(|exe| exe.eq_ignore_ascii_case(&wanted)).cloned()
    };
    if let Some(picked) = picked.and_then(present) {
        return Some(GameExe {
            relative: picked,
            reason: ExeReason::Picked,
        });
    }
    for launcher in listed.iter().filter_map(|listed| present(listed)) {
        if let Some(game) = unreal_game_for(&launcher, exes) {
            return Some(GameExe {
                relative: game,
                reason: ExeReason::UnrealShipping { launcher },
            });
        }
        if !is_helper(&launcher) {
            return Some(GameExe {
                relative: launcher,
                reason: ExeReason::SteamListed,
            });
        }
    }
    // Steam names nothing that is here (another folder layout, or no launch list at all).
    let shipping: Vec<&String> = exes.iter().filter(|path| is_shipping(path)).collect();
    if let [only] = shipping.as_slice() {
        return Some(GameExe {
            relative: (*only).clone(),
            reason: ExeReason::OnlyExe,
        });
    }
    let games: Vec<&String> = exes.iter().filter(|path| !is_helper(path)).collect();
    match games.as_slice() {
        [only] => Some(GameExe {
            relative: (*only).clone(),
            reason: ExeReason::OnlyExe,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn an_unreal_starter_leads_to_the_shipping_exe() {
        let exes = list(&[
            "Engine/Binaries/Win64/CrashReportClient.exe",
            "Engine/Binaries/ThirdParty/CEF3/Win64/UnrealCEFSubProcess.exe",
            "Permafrost.exe",
            "Permafrost/Binaries/Win64/Permafrost-Win64-Shipping.exe",
        ]);
        let game = find_game_exe(&exes, &list(&["Permafrost.exe"]), None).unwrap();
        assert_eq!(
            game.relative,
            "Permafrost/Binaries/Win64/Permafrost-Win64-Shipping.exe"
        );
        assert_eq!(
            game.reason,
            ExeReason::UnrealShipping {
                launcher: "Permafrost.exe".to_owned()
            }
        );
    }

    #[test]
    fn the_shipping_exe_named_after_the_starter_wins_over_a_server_and_others() {
        let exes = list(&[
            "Game.exe",
            "Game/Binaries/Win64/Game-Win64-Shipping.exe",
            "Game/Binaries/Win64/GameServer-Win64-Shipping.exe",
            "Game/Binaries/Win64/Benchmark-Win64-Shipping.exe",
        ]);
        let game = find_game_exe(&exes, &list(&["Game.exe"]), None).unwrap();
        assert_eq!(game.relative, "Game/Binaries/Win64/Game-Win64-Shipping.exe");
    }

    #[test]
    fn a_game_steam_starts_directly_stays_as_it_is() {
        let exes = list(&[
            "Bin64/Game.exe",
            "Bin64/CrashReporter.exe",
            "_CommonRedist/vcredist_x64.exe",
        ]);
        assert_eq!(
            find_game_exe(&exes, &list(&["Bin64/Game.exe"]), None),
            Some(GameExe {
                relative: "Bin64/Game.exe".to_owned(),
                reason: ExeReason::SteamListed
            })
        );
        // Steam's list says it with backslashes and another case; the folder decides the spelling.
        assert_eq!(
            find_game_exe(&exes, &list(&["bin64\\game.exe"]), None)
                .unwrap()
                .relative,
            "Bin64/Game.exe"
        );
        // Listed directly as the shipping exe: that is the game.
        let unreal = list(&["Game/Binaries/Win64/Game-Win64-Shipping.exe"]);
        assert_eq!(
            find_game_exe(&unreal, &unreal, None).unwrap().reason,
            ExeReason::SteamListed
        );
    }

    #[test]
    fn the_users_choice_comes_first_when_it_is_there() {
        let exes = list(&[
            "Game.exe",
            "Game/Binaries/Win64/Game-Win64-Shipping.exe",
            "Tools/Editor.exe",
        ]);
        let game = find_game_exe(&exes, &list(&["Game.exe"]), Some("Tools/Editor.exe")).unwrap();
        assert_eq!(
            (game.relative.as_str(), game.reason),
            ("Tools/Editor.exe", ExeReason::Picked)
        );
        // A choice that is not in this folder (another install) does not count.
        assert_eq!(
            find_game_exe(&exes, &list(&["Game.exe"]), Some("Elsewhere.exe"))
                .unwrap()
                .reason,
            ExeReason::UnrealShipping {
                launcher: "Game.exe".to_owned()
            }
        );
    }

    #[test]
    fn without_a_launch_list_only_an_unmistakable_exe_is_taken() {
        let one = list(&["Game.exe", "unins000.exe", "_CommonRedist/DirectX/DXSETUP.exe"]);
        assert_eq!(
            find_game_exe(&one, &[], None),
            Some(GameExe {
                relative: "Game.exe".to_owned(),
                reason: ExeReason::OnlyExe
            })
        );
        let unreal = list(&["Game.exe", "Game/Binaries/Win64/Game-Win64-Shipping.exe"]);
        assert_eq!(
            find_game_exe(&unreal, &[], None).unwrap().relative,
            "Game/Binaries/Win64/Game-Win64-Shipping.exe"
        );
        assert_eq!(find_game_exe(&list(&["A.exe", "B.exe"]), &[], None), None);
        assert_eq!(find_game_exe(&[], &list(&["Game.exe"]), None), None);
    }

    #[test]
    fn the_folder_walk_finds_exes_with_forward_slashes() {
        let root = tempfile::tempdir().unwrap();
        let shipping = root.path().join("Game/Binaries/Win64");
        std::fs::create_dir_all(&shipping).unwrap();
        std::fs::write(root.path().join("Game.exe"), b"MZ").unwrap();
        std::fs::write(shipping.join("Game-Win64-Shipping.EXE"), b"MZ").unwrap();
        std::fs::write(shipping.join("readme.txt"), b"").unwrap();
        assert_eq!(
            exes_under(root.path()),
            ["Game.exe", "Game/Binaries/Win64/Game-Win64-Shipping.EXE"]
        );
    }
}
