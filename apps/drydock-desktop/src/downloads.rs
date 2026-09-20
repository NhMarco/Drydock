//! Depot worker protocol and orchestration, independent of egui rendering.
use drydock_core::{CdnClient, DepotData, DownloadProgress, ProxyClient, depot, fetch_install_dir};

/// How hard a depot job may push the network and the disk, from Settings.
#[derive(Clone, Copy, Debug)]
pub(crate) struct JobLimits {
    /// Parallel CDN connections for a download.
    pub(crate) connections: usize,
    /// Aggregate download cap in bytes per second.
    pub(crate) max_bps: Option<u64>,
    /// The verify thread setting; `0` lets the engine choose for the drive.
    pub(crate) verify_threads: u32,
}
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver},
    },
    time::Instant,
};

pub(crate) fn installed_directory(
    settings: &drydock_core::Settings,
    manifests: &[drydock_core::SteamManifest],
    app_id: u32,
) -> Option<PathBuf> {
    manifests
        .iter()
        .find(|manifest| manifest.app_id == app_id)
        .map(drydock_core::SteamManifest::install_dir)
        .or_else(|| {
            settings
                .installed_games
                .get(&app_id)
                .map(|game| PathBuf::from(&game.install_dir))
        })
}
/// A running (or just-finished) depot download or verify, driven by a background thread.
pub(crate) struct DownloadJob {
    pub(crate) install_root: Option<PathBuf>,
    /// For a verify: whether every file and chunk checked out, once the job has said so.
    pub(crate) verified: Option<bool>,
    pub(crate) app_id: u32,
    pub(crate) name: String,
    pub(crate) kind: DownloadKind,
    pub(crate) cancel: Arc<AtomicBool>,
    /// `JOB_PREPARING` / `JOB_RUNNING` / `JOB_ABANDONED` — see those constants.
    pub(crate) state: Arc<AtomicU8>,
    pub(crate) receiver: Receiver<DownloadUpdate>,
    pub(crate) progress: Option<DownloadProgress>,
    /// What the job last said it was waiting for, and when it was spawned — the banner shows both
    /// while it is preparing, so a slow depot package looks like work rather than a hang.
    pub(crate) preparing: Option<&'static str>,
    pub(crate) started: Instant,
    /// Smoothed rates in bytes/sec with their running peaks: `speed`/`peak` are what came off the
    /// wire, `disk` is what the job moved on disk (written while downloading, read while verifying).
    /// `sample` is the last (time, network bytes, disk bytes) the estimates were taken from.
    pub(crate) speed_bps: f64,
    pub(crate) peak_bps: f64,
    pub(crate) disk_bps: f64,
    pub(crate) peak_disk_bps: f64,
    pub(crate) sample: Option<(Instant, u64, u64)>,
    /// `Some` once the job ended: `Ok(summary)` or `Err(message)`.
    pub(crate) finished: Option<Result<String, String>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum DownloadKind {
    Download,
    Verify,
}

pub(crate) enum DownloadUpdate {
    Installed(PathBuf),
    /// A verify finished; `true` when nothing needs repair.
    Verified(bool),
    /// What the job is waiting for before the first progress tick — the "Preparing…" phase.
    Preparing(&'static str),
    Progress(DownloadProgress),
    Finished(Result<String, String>),
}

/// What a job reports when it was stopped before the engine started. A deliberate pause keeps it
/// out of the banner (see `poll_download`), so it only ever shows up after an unexpected stop.
pub(crate) const STOPPED_WHILE_PREPARING: &str = "Stopped before the download started.";

/// How far a job has got, shared between its thread and the UI.
///
/// The UI is allowed to let go of a job that is still preparing — a depot package the source builds
/// on demand can take minutes, and nobody may be locked into waiting for it. It must never let go of
/// one that is already writing files, or a resume would put a second engine on the same files. The
/// thread claims [`JOB_RUNNING`] before it touches anything and the UI claims [`JOB_ABANDONED`]
/// before it drops the job; the swap has exactly one winner, so the two can never both proceed.
pub(crate) const JOB_PREPARING: u8 = 0;
pub(crate) const JOB_RUNNING: u8 = 1;
pub(crate) const JOB_ABANDONED: u8 = 2;

/// The thread's half of the hand-off: take the job before writing anything. `false` means the UI
/// has let go of it, so this thread must stop without touching the install folder.
pub(crate) fn claim_running(state: &AtomicU8) -> bool {
    claim(state, JOB_RUNNING)
}

/// The UI's half: take the job away from its thread while it is still preparing. `false` means the
/// thread is already writing, so it has to be stopped the ordinary way (it ends between chunks).
pub(crate) fn claim_abandoned(state: &AtomicU8) -> bool {
    claim(state, JOB_ABANDONED)
}

fn claim(state: &AtomicU8, to: u8) -> bool {
    state
        .compare_exchange(JOB_PREPARING, to, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Where a depot download for `app_id` writes.
///
/// A known install always wins — the folder Steam tracks, else the one registered in Drydock's
/// library (see [`installed_directory`]) — so an update or repair lands on the existing files rather
/// than starting a second copy elsewhere. Only a fresh install is free to choose, and then Drydock's
/// configured games folder takes precedence over Steam's `steamapps\common`. The worker reports the
/// folder it used back to the UI, which registers exactly that one.
pub(crate) fn depot_install_root(
    app_id: u32,
    installed_dir: Option<PathBuf>,
    games_directory: Option<PathBuf>,
    steam_root: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(dir) = installed_dir {
        return Ok(dir);
    }
    let installdir = fetch_install_dir(app_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Steam did not report an install folder for this game.".to_owned())?;
    match games_directory {
        Some(games) => Ok(games.join(installdir)),
        None => {
            let root = steam_root.ok_or_else(|| {
                "No games folder is set and Steam was not found. Set either one in Settings.".to_owned()
            })?;
            Ok(root.join("steamapps").join("common").join(installdir))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_depot_job(
    app_id: u32,
    name: &str,
    kind: DownloadKind,
    steam_root: Option<PathBuf>,
    installed_dir: Option<PathBuf>,
    games_directory: Option<PathBuf>,
    limits: JobLimits,
    cancel: &AtomicBool,
    state: &AtomicU8,
    sender: &mpsc::Sender<DownloadUpdate>,
) -> Result<String, String> {
    // Everything before the engine starts is a network request, and the depot package is built on
    // demand upstream — minutes for a big title. So each step says what it is waiting for, is
    // stoppable in itself, and the cancel flag is checked again in between: Pause/Cancel ends the
    // job right here instead of only once chunks are moving.
    let step = |what: &'static str| {
        let _ = sender.send(DownloadUpdate::Preparing(what));
    };
    let stopped = || {
        if cancel.load(Ordering::Relaxed) {
            Err(STOPPED_WHILE_PREPARING.to_owned())
        } else {
            Ok(())
        }
    };

    step("Fetching the depot package…");
    let proxy = ProxyClient::new().map_err(|error| error.to_string())?;
    let mut data = DepotData::fetch_cancellable(&proxy, app_id, cancel).map_err(|error| error.to_string())?;
    stopped()?;

    // A package carries every depot the app has — the Windows, macOS and Linux builds of the same
    // game, each its full size. Only what Windows installs is downloaded (and verified): the rest
    // would take several times the disk space and make a verify report every file of it as missing.
    step("Checking which depots Windows installs…");
    if let Ok(foreign) = drydock_core::fetch_non_windows_depots(app_id) {
        data.drop_depots(&foreign);
    }
    stopped()?;

    // Refuse to "succeed" on a package that has no game content. Some upstream builds ship only the
    // shared redistributables (Visual C++, DirectX, …) with keys for the real content depots but no
    // manifest for them — downloading that would leave the game unplayable while claiming it
    // finished. Tell the user to retry once the source has packaged the content.
    if matches!(kind, DownloadKind::Download) && data.has_no_content() {
        return Err(format!(
            "{name} isn't fully available from the source yet: only the shared redistributables were \
             packaged (no game content depots). The upstream is likely still building the package — \
             try Download again in a few minutes."
        ));
    }

    step("Choosing the install folder…");
    let install_root = depot_install_root(app_id, installed_dir, games_directory, steam_root)?;
    stopped()?;

    // Claim the job before a single file is touched. The UI may have let go of it while it was
    // preparing — then the job is no longer this thread's and it must not write anything, because
    // the user can already have started the same download again.
    if !claim_running(state) {
        return Err(STOPPED_WHILE_PREPARING.to_owned());
    }

    let forward = |progress: DownloadProgress| {
        let _ = sender.send(DownloadUpdate::Progress(progress));
    };
    match kind {
        DownloadKind::Download => {
            let cdn = CdnClient::new().map_err(|error| error.to_string())?;
            let outcome = depot::download::download(
                &data,
                &install_root,
                &cdn,
                cancel,
                limits.connections,
                limits.max_bps,
                forward,
            )
            .map_err(|error| error.to_string())?;
            let _ = sender.send(DownloadUpdate::Installed(outcome.install_root));
            Ok(format!(
                "Downloaded {name} — {} files, {}",
                outcome.files_written,
                human_bytes(outcome.bytes_written)
            ))
        }
        DownloadKind::Verify => {
            let threads = depot::verify_threads(limits.verify_threads, &install_root);
            let outcome = depot::verify(&data, &install_root, cancel, threads, forward)
                .map_err(|error| error.to_string())?;
            let _ = sender.send(DownloadUpdate::Verified(outcome.is_complete()));
            if outcome.is_complete() {
                Ok(format!(
                    "{name} verified — all {} chunks OK",
                    outcome.total_chunks
                ))
            } else {
                Ok(format!(
                    "{name}: {} of {} chunks and {} files need repair — press Download to fix",
                    outcome.bad_chunks, outcome.total_chunks, outcome.bad_files
                ))
            }
        }
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manually_registered_directory_survives_standard_folder_change() {
        let mut settings = drydock_core::Settings::default();
        settings.installed_games.insert(
            1,
            drydock_core::InstalledGame {
                name: "Game".into(),
                install_dir: "custom/game".into(),
            },
        );
        settings.games_directory = "new/default".into();
        assert_eq!(
            installed_directory(&settings, &[], 1),
            Some(PathBuf::from("custom/game"))
        );
    }
    #[test]
    fn a_job_is_either_let_go_of_or_running_but_never_both() {
        // The UI got there first: the thread must not start writing into a folder the user may
        // already be downloading into again.
        let state = AtomicU8::new(JOB_PREPARING);
        assert!(claim_abandoned(&state));
        assert!(!claim_running(&state));
        // The thread got there first: the UI has to stop it the ordinary way instead of dropping a
        // job that owns the files.
        let state = AtomicU8::new(JOB_PREPARING);
        assert!(claim_running(&state));
        assert!(!claim_abandoned(&state));
        // And neither claim can be repeated.
        assert!(!claim_running(&state));
    }

    #[test]
    fn registered_target_needs_no_remote_resolution() {
        let target = PathBuf::from("custom/game");
        assert_eq!(
            depot_install_root(1, Some(target.clone()), None, None).unwrap(),
            target
        );
    }
}
