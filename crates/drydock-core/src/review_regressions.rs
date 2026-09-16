use crate::{
    AppPayloadStore, Settings,
    depot::{
        download::{self, DepotData},
        manifest::{ChunkEntry, DepotManifest, FileEntry},
    },
};
use std::{collections::BTreeMap, fs, sync::atomic::AtomicBool};

#[test]
fn unicode_plugin_names_do_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("config/stplug-in");
    fs::create_dir_all(&plugin).unwrap();
    for name in ["备份说明", "é", "🎮🎮", "42.LUA"] {
        fs::write(plugin.join(name), "test").unwrap();
    }
    assert_eq!(crate::steam_service::installed_app_luas(dir.path()), [42].into());
}

#[test]
fn remote_metadata_rejects_escaping_paths() {
    for device in ["CON", "aux.dll", "NUL", "com1.txt", "LPT².log"] {
        assert!(!crate::safe_path::is_portable_path_segment(device));
    }
    for path in [
        "../outside",
        "C:/outside",
        "C:outside",
        "\\\\server\\share",
        "a\\..\\b",
        "file:ads",
        "a\0b",
    ] {
        let json = serde_json::json!({"data":{"1":{"config":{"installdir":path,"launch":{"0":{"executable":format!("{path}.exe")}}}}}});
        assert!(
            crate::steam_appinfo::install_dir_name(&json, 1).is_none(),
            "{path}"
        );
        assert!(
            crate::steam_appinfo::windows_executables(&json, 1).is_empty(),
            "{path}"
        );
    }
}

#[test]
fn verify_checks_lengths_and_empty_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("data.bin"), b"goodEXTRA").unwrap();
    let data = DepotData {
        app_id: 1,
        manifests: vec![DepotManifest {
            depot_id: 1,
            manifest_gid: 1,
            filenames_encrypted: false,
            files: vec![
                FileEntry {
                    path: "data.bin".into(),
                    size: 4,
                    flags: 0,
                    chunks: vec![ChunkEntry {
                        sha: [0; 20],
                        crc: crate::depot::crypto::steam_adler_hash(b"good"),
                        offset: 0,
                        uncompressed_len: 4,
                        compressed_len: 4,
                    }],
                },
                FileEntry {
                    path: "empty".into(),
                    size: 0,
                    flags: 0,
                    chunks: vec![],
                },
            ],
        }],
        ..Default::default()
    };
    let outcome = crate::depot::verify(&data, dir.path(), &AtomicBool::new(false), 2, |_| {}).unwrap();
    assert_eq!(outcome.bad_chunks, 0);
    assert_eq!(outcome.bad_files, 2);
    assert!(!outcome.is_complete());
    assert_eq!(download::additional_space(&data, dir.path()), 0);
    fs::write(dir.path().join("data.bin"), b"g").unwrap();
    assert_eq!(download::additional_space(&data, dir.path()), 3);
    fs::write(dir.path().join("data.bin"), b"good").unwrap();
    fs::write(dir.path().join("empty"), b"").unwrap();
    assert!(
        crate::depot::verify(&data, dir.path(), &AtomicBool::new(false), 2, |_| {})
            .unwrap()
            .is_complete()
    );
}

#[test]
fn payload_failure_and_interrupted_publish_preserve_old_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppPayloadStore::new(dir.path());
    store
        .save(1, Some(("1.lua", b"original")), &BTreeMap::new())
        .unwrap();
    assert!(
        store
            .save(1, None, &BTreeMap::from([("invalid\0.manifest".into(), vec![1])]))
            .is_err()
    );
    assert_eq!(store.load(1).lua.unwrap().1, b"original");
    fs::rename(store.app_directory(1), dir.path().join("apps/1.bak")).unwrap();
    assert_eq!(store.load(1).lua.unwrap().1, b"original");
    store.save(1, Some(("1.lua", b"next")), &BTreeMap::new()).unwrap();
    assert_eq!(store.load(1).lua.unwrap().1, b"next");
    // Once the new generation is in place the previous one is not kept around.
    store.save(1, Some(("1.lua", b"last")), &BTreeMap::new()).unwrap();
    assert!(!dir.path().join("apps/1.bak").exists());
    assert_eq!(store.load(1).lua.unwrap().1, b"last");
}

fn denuvo_fix() -> crate::mfb::DenuvoFix {
    crate::mfb::DenuvoFix {
        lua: crate::mfb::RepositoryFile {
            relative_path: "1.lua".into(),
            source_url: String::new(),
            sha: String::new(),
        },
        zip_parts: vec![],
    }
}

#[test]
fn failed_fix_does_not_replace_lua() {
    let dir = tempfile::tempdir().unwrap();
    // A real Steam folder, so the Lua install itself would succeed.
    fs::write(dir.path().join("steam.exe"), b"").unwrap();
    fs::create_dir_all(dir.path().join("config/stplug-in")).unwrap();
    fs::write(dir.path().join("config/stplug-in/1.lua"), b"original").unwrap();
    fs::write(dir.path().join("bad.zip"), b"invalid archive").unwrap();
    let game = dir.path().join("game");
    fs::create_dir(&game).unwrap();
    assert!(
        crate::fixes::apply_denuvo_fix(
            dir.path(),
            &game,
            &denuvo_fix(),
            b"new",
            &dir.path().join("bad.zip")
        )
        .is_err()
    );
    assert_eq!(
        fs::read(dir.path().join("config/stplug-in/1.lua")).unwrap(),
        b"original"
    );
}

#[test]
fn a_fix_whose_lua_cannot_be_installed_restores_the_game_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("steam.exe"), b"").unwrap();
    // A folder where the Lua belongs makes installing it fail after the game files were replaced.
    fs::create_dir_all(dir.path().join("config/stplug-in/1.lua")).unwrap();
    let game = dir.path().join("game");
    fs::create_dir_all(game.join("bin")).unwrap();
    fs::write(game.join("bin/game.exe"), b"original").unwrap();
    let zip = crate::emu_template::zip_files(&[
        ("bin/game.exe".into(), b"cracked".to_vec()),
        ("bin/added.dll".into(), b"new".to_vec()),
        ("plugins/extra/added.dll".into(), b"new".to_vec()),
    ])
    .unwrap();
    fs::write(dir.path().join("fix.zip"), zip).unwrap();
    assert!(
        crate::fixes::apply_denuvo_fix(
            dir.path(),
            &game,
            &denuvo_fix(),
            b"new",
            &dir.path().join("fix.zip")
        )
        .is_err()
    );
    assert_eq!(fs::read(game.join("bin/game.exe")).unwrap(), b"original");
    assert!(!game.join("bin/added.dll").exists());
    // Neither the folders the fix created, the staged archive nor the backups are left behind.
    let leftovers: Vec<String> = fs::read_dir(&game)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["bin"]);
}

#[test]
fn an_applied_fix_creates_its_folders_replaces_files_and_installs_the_lua() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("steam.exe"), b"").unwrap();
    let game = dir.path().join("game");
    fs::create_dir(&game).unwrap();
    fs::write(game.join("game.exe"), b"original").unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
    zip.add_directory("saves/", options).unwrap();
    zip.start_file("game.exe", options).unwrap();
    zip.write_all(b"cracked").unwrap();
    fs::write(dir.path().join("fix.zip"), zip.finish().unwrap().into_inner()).unwrap();

    let replaced = crate::fixes::apply_denuvo_fix(
        dir.path(),
        &game,
        &denuvo_fix(),
        b"new",
        &dir.path().join("fix.zip"),
    )
    .unwrap();
    assert_eq!(replaced, 1);
    assert_eq!(fs::read(game.join("game.exe")).unwrap(), b"cracked");
    assert!(
        game.join("saves").is_dir(),
        "an empty folder in the archive is created too"
    );
    assert_eq!(
        fs::read(dir.path().join("config/stplug-in/1.lua")).unwrap(),
        b"new"
    );
    let mut left: Vec<String> = fs::read_dir(&game)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["game.exe", "saves"]);
}

#[test]
fn consecutive_completed_jobs_register_their_actual_roots() {
    let mut settings = Settings::default();
    for id in [1, 2] {
        settings.download_queue.push(crate::settings::QueuedDownload {
            app_id: id,
            name: id.to_string(),
        });
    }
    for id in [1, 2] {
        crate::download_queue::register_completed(
            &mut settings,
            id,
            &id.to_string(),
            std::path::Path::new("custom/game"),
        );
    }
    assert!(settings.download_queue.is_empty());
    assert_eq!(settings.installed_games.len(), 2);
    assert_eq!(settings.installed_games[&1].install_dir, "custom/game");
}

#[test]
fn archive_rejects_existing_directory_link() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game");
    let outside = dir.path().join("outside");
    fs::create_dir(&game).unwrap();
    fs::create_dir(&outside).unwrap();
    crate::safe_path::link_folder(&outside, &game.join("link"));
    let zip = crate::emu_template::zip_files(&[("link/probe".into(), b"test".to_vec())]).unwrap();
    assert!(crate::ubisoft::install_magicfiles(&zip, &game).is_err());
    assert!(!outside.join("probe").exists());
}
