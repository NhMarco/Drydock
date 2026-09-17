//! Depot worker protocol and orchestration, independent of egui rendering.
use drydock_core::{CdnClient, DepotData, DownloadProgress, ProxyClient, depot, fetch_install_dir};

/// How hard a depot job may push the network and the disk, from Settings.
#[derive(Clone, Copy, Debug)]
pub struct JobLimits {
    /// Parallel CDN connections for a download.
    pub connections: usize,
    /// Aggregate download cap in bytes per second.
    pub max_bps: Option<u64>,
    /// The verify thread setting; `0` lets the engine choose for the drive.
    pub verify_threads: u32,
}
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{self, Receiver},
    },
    time::Instant,
};

pub fn installed_directory(
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
pub struct DownloadJob {
    pub install_root: Option<PathBuf>,
    /// For a verify: whether every file and chunk checked out, once the job has said so.
    pub verified: Option<bool>,
    pub app_id: u32,
    pub name: String,
    pub kind: DownloadKind,
    pub cancel: Arc<AtomicBool>,
    pub receiver: Receiver<DownloadUpdate>,
    pub progress: Option<DownloadProgress>,
    /// Smoothed download speed in bytes/sec, its running peak, plus the last (time, done_bytes)
    /// sample the estimate came from.
    pub speed_bps: f64,
    pub peak_bps: f64,
    pub sample: Option<(Instant, u64)>,
    /// `Some` once the job ended: `Ok(summary)` or `Err(message)`.
    pub finished: Option<Result<String, String>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DownloadKind {
    Download,
    Verify,
}

pub enum DownloadUpdate {
    Installed(PathBuf),
    /// A verify finished; `true` when nothing needs repair.
    Verified(bool),
    Progress(DownloadProgress),
    Finished(Result<String, String>),
}

/// Where a depot download for `app_id` writes.
///
/// A known install always wins — the folder Steam tracks, else the one registered in Drydock's
/// library (see [`installed_directory`]) — so an update or repair lands on the existing files rather
/// than starting a second copy elsewhere. Only a fresh install is free to choose, and then Drydock's
/// configured games folder takes precedence over Steam's `steamapps\common`. The worker reports the
/// folder it used back to the UI, which registers exactly that one.
pub fn depot_install_root(
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
pub fn run_depot_job(
    app_id: u32,
    name: &str,
    kind: DownloadKind,
    steam_root: Option<PathBuf>,
    installed_dir: Option<PathBuf>,
    games_directory: Option<PathBuf>,
    limits: JobLimits,
    cancel: &AtomicBool,
    sender: &mpsc::Sender<DownloadUpdate>,
) -> Result<String, String> {
    let proxy = ProxyClient::new().map_err(|error| error.to_string())?;
    let data = DepotData::fetch(&proxy, app_id).map_err(|error| error.to_string())?;

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

    let install_root = depot_install_root(app_id, installed_dir, games_directory, steam_root)?;

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

pub fn human_bytes(bytes: u64) -> String {
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
    fn registered_target_needs_no_remote_resolution() {
        let target = PathBuf::from("custom/game");
        assert_eq!(
            depot_install_root(1, Some(target.clone()), None, None).unwrap(),
            target
        );
    }
}
