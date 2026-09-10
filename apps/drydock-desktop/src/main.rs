#![cfg_attr(
    all(target_os = "windows", not(feature = "screenshot")),
    windows_subsystem = "windows"
)]

mod image_cache;
mod ui;

#[cfg(feature = "screenshot")]
mod screenshot;

use drydock_core::{
    APP_VERSION, AppUpdater, PortablePaths, Settings, ensure_elevated, harden_dll_search, run_self_test,
};
use eframe::egui;

/// Appends a crash report to `crash.log` in the per-user data directory, capped so a crash loop
/// cannot fill the disk. Returns the log path so the hook can point at it on stderr.
fn write_crash_log(report: &str) -> Option<std::path::PathBuf> {
    use std::io::Write as _;

    let path = PortablePaths::discover().ok()?.settings_dir().join("crash.log");
    // Roll the file over once it passes ~1 MB; keeping one previous generation is enough to catch
    // "it crashed again right after the first one" without unbounded growth.
    const MAXIMUM_LOG_BYTES: u64 = 1024 * 1024;
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAXIMUM_LOG_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    file.write_all(report.as_bytes()).ok()?;
    Some(path)
}

fn main() -> eframe::Result {
    // Install a panic hook that records crashes to a file as well as stderr. Release builds run on
    // the Windows GUI subsystem and have no console attached, so `eprintln!` alone would send every
    // production crash report — exactly the ones worth diagnosing — nowhere.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let report = format!(
            "==== Drydock {APP_VERSION} panicked ====\n\
             when:   {:?}\n\
             thread: {name}\n\
             message:{payload}\n\
             at:     {location}\n{}\n",
            std::time::SystemTime::now(),
            std::backtrace::Backtrace::force_capture()
        );
        eprint!("{report}");
        if let Some(path) = write_crash_log(&report) {
            eprintln!("Crash report written to {}", path.display());
        }
        default_hook(info);
    }));
    // Before anything can trigger a DLL load, lock resolution to System32 so a stray DLL next to
    // the executable can never hijack the load (the 0xc000007b-on-launch class of failure).
    harden_dll_search();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "screenshot")]
    if let Some(directory) = arguments
        .iter()
        .position(|argument| argument == "--screenshot")
        .and_then(|index| arguments.get(index + 1))
    {
        return screenshot::run(std::path::PathBuf::from(directory));
    }
    if arguments.as_slice() == ["--self-test"] {
        match run_self_test() {
            Ok(report) => {
                println!(
                    "Drydock self-test passed: target={}, catalog_apps={}, activation={}, settings={}",
                    report.target,
                    report.catalog_apps,
                    report.activation_request_created,
                    report.settings_round_trip
                );
                return Ok(());
            }
            Err(error) => {
                eprintln!("Drydock self-test failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if arguments.as_slice() == ["--version"] {
        println!("{APP_VERSION}");
        return Ok(());
    }
    // Diagnostic: print the resolved configuration and where each value came from. The first thing
    // to run when a self-hosted setup talks to the wrong proxy — or to nothing at all. Secrets are
    // redacted, so the output is safe to paste into a bug report.
    if arguments.as_slice() == ["--config"] {
        // Settings-level overrides are part of the answer, so load them the same way the GUI does.
        match PortablePaths::discover() {
            Ok(paths) => {
                let (settings, outcome) = Settings::load_recovering(&paths.settings_file());
                settings.apply_config_overrides();
                println!("Data directory: {}", paths.settings_dir().display());
                if !matches!(outcome, drydock_core::LoadOutcome::Loaded) {
                    println!("Settings:       {outcome:?}");
                }
            }
            Err(error) => println!("Data directory: unavailable ({error})"),
        }
        println!("Version:        {APP_VERSION}");
        println!();
        for entry in drydock_core::describe_config() {
            println!(
                "{:<18} {:<44} [{}, {}]",
                entry.name,
                entry.value,
                entry.source.label(),
                entry.env_var
            );
        }
        return Ok(());
    }
    // Diagnostic: run the full update check (download + verify + stage) without applying it.
    if arguments.as_slice() == ["--check-update"] {
        match AppUpdater::new().and_then(|updater| updater.prepare_update()) {
            Ok(Some(update)) => {
                println!(
                    "Update available: {} (verified sha256 {}, staged at {})",
                    update.version,
                    update.sha256,
                    update.source_path.display()
                );
                return Ok(());
            }
            Ok(None) => {
                println!("Up to date: {APP_VERSION} is the latest release.");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Update check failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if AppUpdater::try_apply_from_args(&arguments) {
        return Ok(());
    }
    if !cfg!(debug_assertions) && !ensure_elevated(&arguments).unwrap_or(false) {
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Drydock")
        .with_position([48.0, 48.0])
        .with_inner_size([1080.0, 640.0])
        .with_min_inner_size([800.0, 500.0])
        .with_clamp_size_to_monitor_size(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    // vsync off: the frame rate is capped to a fixed 60 FPS in the update loop instead (see
    // `DrydockApp::update`), so a high-refresh monitor doesn't render at 144+ FPS and the cap stays 60
    // regardless of the display. (In eframe 0.36 vsync lives under the glow renderer options.)
    options.glow_options.vsync = false;

    eframe::run_native(
        "Drydock",
        options,
        Box::new(|context| Ok(Box::new(ui::DrydockApp::new(context)))),
    )
}
