use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use drydock_core::*;
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};

use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;
use crate::ui::helpers::*;

pub fn run_depot_job(
    app_id: u32,
    name: &str,
    kind: DownloadKind,
    steam_root: Option<PathBuf>,
    installed_dir: Option<PathBuf>,
    connections: usize,
    max_bps: Option<u64>,
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

    let install_root = match installed_dir {
        Some(dir) => dir,
        None => {
            let root = steam_root.ok_or_else(|| "Steam folder not found. Set it in Settings.".to_owned())?;
            let installdir = fetch_install_dir(app_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Steam did not report an install folder for this game.".to_owned())?;
            root.join("steamapps").join("common").join(installdir)
        }
    };

    let forward = |progress: DownloadProgress| {
        let _ = sender.send(DownloadUpdate::Progress(progress));
    };
    match kind {
        DownloadKind::Download => {
            let cdn = CdnClient::new().map_err(|error| error.to_string())?;
            let outcome =
                depot::download::download(&data, &install_root, &cdn, cancel, connections, max_bps, forward)
                    .map_err(|error| error.to_string())?;
            Ok(format!(
                "Downloaded {name} — {} files, {}",
                outcome.files_written,
                human_bytes(outcome.bytes_written)
            ))
        }
        DownloadKind::Verify => {
            let outcome = depot::download::verify(&data, &install_root, cancel, forward)
                .map_err(|error| error.to_string())?;
            if outcome.is_complete() {
                Ok(format!(
                    "{name} verified — all {} chunks OK",
                    outcome.total_chunks
                ))
            } else {
                Ok(format!(
                    "{name}: {} of {} chunks need repair — press Download to fix",
                    outcome.bad_chunks, outcome.total_chunks
                ))
            }
        }
    }
}

/// Formats a byte-per-second rate as a compact human string (e.g. `9.9 MB/s`).

pub fn download_mini_stat(ui: &mut egui::Ui, caption: &str, value: &str, accent: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(RichText::new(caption).size(9.0).strong().color(accent));
        ui.label(RichText::new(value).size(15.0).strong().color(TEXT));
    });
}

/// Keeps only the last `max` characters of `text`, prefixing an ellipsis when it was truncated.

impl DrydockApp {
    pub fn download_running(&self) -> bool {
        self.download_job
            .as_ref()
            .is_some_and(|job| job.finished.is_none())
    }

    /// Spawns the background thread for one depot job and makes it the active `download_job`. Callers
    /// (queue start / verify) guarantee nothing else is running.
    pub fn spawn_job(&mut self, app_id: u32, name: String, kind: DownloadKind) {
        let steam_root = self.steam.root.clone();
        let installed_dir = self
            .manifests
            .iter()
            .find(|manifest| manifest.app_id == app_id)
            .map(SteamManifest::install_dir);
        // Parallel connections (0 = fall back to the default 8) and an optional MB/s cap, from Settings.
        let connections = match self.settings.max_download_connections {
            0 => 8,
            n => n.clamp(1, 32),
        } as usize;
        let max_bps = match self.settings.max_download_mbps {
            0 => None,
            mbps => Some(u64::from(mbps) * 1024 * 1024),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let job_name = name.clone();
        std::thread::spawn(move || {
            let result = run_depot_job(
                app_id,
                &job_name,
                kind,
                steam_root,
                installed_dir,
                connections,
                max_bps,
                &thread_cancel,
                &sender,
            );
            let _ = sender.send(DownloadUpdate::Finished(result));
        });
        self.download_job = Some(DownloadJob {
            app_id,
            name,
            kind,
            cancel,
            receiver,
            progress: None,
            speed_bps: 0.0,
            peak_bps: 0.0,
            sample: None,
            finished: None,
        });
    }

    /// Applies a [`QueueEffect`] from [`drydock_core::download_queue`]: persist the reordered queue and
    /// then start, switch or leave the worker thread alone as the effect requires.
    ///
    /// The ordering rules themselves live in core (and are unit-tested there); this is only the part
    /// that owns threads and the settings file.
    pub fn apply_queue_effect(&mut self, effect: QueueEffect, switching_status: &str) {
        match effect {
            QueueEffect::Unchanged => {}
            QueueEffect::PersistOnly => {
                let _ = self.persist_settings();
            }
            QueueEffect::StartFront => {
                let _ = self.persist_settings();
                self.download_paused = false;
                self.download_error = None;
                self.start_front_download();
            }
            QueueEffect::SwitchToFront => {
                let _ = self.persist_settings();
                self.switch_or_start(switching_status);
            }
        }
    }

    /// Adds a game to the persistent download queue and starts it when nothing else is downloading.
    /// A game already in the queue isn't added twice.
    pub fn enqueue_download(&mut self, app_id: u32, name: String) {
        let busy = self.download_running() || self.download_paused;
        let effect = download_queue::enqueue(
            &mut self.settings.download_queue,
            QueuedDownload {
                app_id,
                name: name.clone(),
            },
            busy,
        );
        match effect {
            QueueEffect::Unchanged => {
                self.status = format!("{name} is already in the download queue.");
                self.status_error = false;
            }
            QueueEffect::PersistOnly => {
                let _ = self.persist_settings();
                self.status = format!("Queued {name} for download.");
                self.status_error = false;
            }
            _ => self.apply_queue_effect(effect, "Switching download…"),
        }
    }

    /// Starts (or resumes) the download at the front of the queue. The depot engine skips chunks that
    /// already verify on disk, so this resumes an interrupted download where it left off.
    pub fn start_front_download(&mut self) {
        if self.download_running() {
            return;
        }
        let Some(front) = self.settings.download_queue.first().cloned() else {
            return;
        };
        self.download_paused = false;
        self.download_error = None;
        self.spawn_job(front.app_id, front.name.clone(), DownloadKind::Download);
        self.status = format!("Downloading {}…", front.name);
        self.status_error = false;
    }

    /// Runs a one-off verify (not queued/persisted) when nothing is downloading.
    pub fn start_verify(&mut self, app_id: u32, name: String) {
        if self.download_running() {
            self.status = "A download is already in progress.".into();
            self.status_error = true;
            return;
        }
        self.spawn_job(app_id, name, DownloadKind::Verify);
        self.status = "Verifying files…".into();
        self.status_error = false;
    }

    /// Pauses the running download: the thread stops cleanly between chunks and the entry stays at the
    /// front of the queue, so partial files remain and it can resume later.
    pub fn pause_download(&mut self) {
        if let Some(job) = self.download_job.as_ref()
            && job.kind == DownloadKind::Download
            && job.finished.is_none()
        {
            job.cancel.store(true, Ordering::Relaxed);
            self.download_paused = true;
            self.status = "Pausing the download…".into();
            self.status_error = false;
        }
    }

    /// Removes the current (paused) download from the queue and starts the next one, if any. Partial
    /// files are left on disk. Only meaningful when the front download is paused (no thread running).
    pub fn remove_current_download(&mut self) {
        let effect = download_queue::remove_front(&mut self.settings.download_queue);
        self.apply_queue_effect(effect, "Switching download…");
    }

    /// Removes a queued (not-current) download by App ID.
    pub fn remove_queued_download(&mut self, app_id: u32) {
        let effect = download_queue::remove_queued(&mut self.settings.download_queue, app_id);
        self.apply_queue_effect(effect, "Switching download…");
    }

    /// Makes a queued download the current one: moves it to the front and starts it. Whatever was
    /// downloading is stopped and stays in the queue right behind it (it resumes when it reaches the
    /// front again).
    pub fn activate_download(&mut self, app_id: u32) {
        let running = self.download_running();
        let effect = download_queue::activate(&mut self.settings.download_queue, app_id, running);
        self.apply_queue_effect(effect, "Switching download…");
    }

    /// Sends the active download to the back of the queue and starts the next one. With nothing else
    /// queued, this just pauses it.
    pub fn demote_current_download(&mut self) {
        let running = self.download_running();
        let effect = download_queue::demote_front(&mut self.settings.download_queue, running);
        if effect == QueueEffect::Unchanged {
            // Fewer than two entries — there is no "back" to move to, so pausing is the useful action.
            self.pause_download();
            return;
        }
        self.apply_queue_effect(effect, "Moving to the back of the queue…");
    }

    /// After the queue was reordered: stop the running thread so the new front starts (a "switch",
    /// not a pause), or start the new front directly when nothing is running.
    pub fn switch_or_start(&mut self, switching_status: &str) {
        if self.download_running() {
            self.download_switch_pending = true;
            if let Some(job) = &self.download_job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.status = switching_status.to_owned();
            self.status_error = false;
        } else {
            self.download_paused = false;
            self.download_error = None;
            self.start_front_download();
        }
    }

    pub fn poll_download(&mut self) {
        let mut finished: Option<Result<String, String>> = None;
        if let Some(job) = self.download_job.as_mut() {
            if job.finished.is_some() {
                return;
            }
            loop {
                match job.receiver.try_recv() {
                    Ok(DownloadUpdate::Progress(progress)) => job.progress = Some(progress),
                    Ok(DownloadUpdate::Finished(result)) => {
                        finished = Some(result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some(Err("The download ended unexpectedly.".into()));
                        break;
                    }
                }
            }
            // Estimate download speed from how many bytes arrived since the last ~0.5s sample.
            if let Some(done) = job.progress.as_ref().map(|p| p.done_bytes) {
                let now = Instant::now();
                match job.sample {
                    Some((then, prev_done)) if now.duration_since(then).as_secs_f64() >= 0.5 => {
                        let elapsed = now.duration_since(then).as_secs_f64();
                        let instant = done.saturating_sub(prev_done) as f64 / elapsed;
                        // Exponential smoothing so the number doesn't jump around.
                        job.speed_bps = if job.speed_bps == 0.0 {
                            instant
                        } else {
                            job.speed_bps * 0.6 + instant * 0.4
                        };
                        job.peak_bps = job.peak_bps.max(job.speed_bps);
                        job.sample = Some((now, done));
                    }
                    None => job.sample = Some((now, done)),
                    _ => {}
                }
            }
            if let Some(result) = finished.clone() {
                job.finished = Some(result);
            }
        }
        // Remember the latest progress so a paused download can still show its position.
        if let Some(progress) = self.download_job.as_ref().and_then(|job| job.progress.clone()) {
            self.download_last = Some(progress);
        }
        if let Some(result) = finished {
            let kind = self.download_job.as_ref().map(|job| job.kind);
            match &result {
                Ok(message) => {
                    self.status = message.clone();
                    self.status_error = false;
                }
                Err(message) => {
                    self.status = message.clone();
                    self.status_error = true;
                }
            }
            if kind == Some(DownloadKind::Download) {
                let was_pause = self.download_paused;
                let switching = std::mem::take(&mut self.download_switch_pending);
                let finished_game = self
                    .download_job
                    .as_ref()
                    .map(|job| (job.app_id, job.name.clone()));
                self.download_job = None; // the download thread has ended
                match result {
                    Ok(_) => {
                        // Completed: drop it from the queue by App ID (robust to reordering), persist,
                        // register it in the Drydock library, and resume the next one.
                        if let Some((id, name)) = finished_game {
                            download_queue::remove_completed(&mut self.settings.download_queue, id);
                            let _ = self.persist_settings();
                            self.start_download_install_detect(id, name);
                        }
                        self.download_paused = false;
                        self.download_error = None;
                        self.refresh_dynamic_state();
                        self.start_front_download();
                    }
                    Err(message) => {
                        if switching {
                            // Cancelled only to switch to a reordered front — the game stays queued at
                            // its new position; start whatever is now at the front.
                            self.download_paused = false;
                            self.download_error = None;
                            self.start_front_download();
                        } else {
                            // A user pause or a real error: keep the entry so it can resume; record the
                            // message only when it wasn't a deliberate pause.
                            self.download_paused = true;
                            self.download_error = if was_pause { None } else { Some(message) };
                        }
                    }
                }
            } else {
                // A one-off verify leaves its result in `download_job` (the banner shows DISMISS).
                self.refresh_dynamic_state();
            }
        }
    }

    pub fn downloads_page(&mut self, ui: &mut egui::Ui) {
        if back_button(ui, "Return to the store").clicked() {
            self.page = Page::Home;
            return;
        }
        ui.add_space(12.0);
        page_heading(ui, "Downloads");
        ui.add_space(16.0);

        // The banner shows a running/finished verify, or the current download (queue front) whether
        // running, paused, or stopped by an error. With neither, there is nothing to download.
        let verify = self
            .download_job
            .as_ref()
            .filter(|job| job.kind == DownloadKind::Verify);
        let front = self.settings.download_queue.first().cloned();
        if verify.is_none() && front.is_none() {
            panel(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("No active downloads.")
                        .size(13.5)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Open a game and press Download to fetch its files here.")
                        .size(11.5)
                        .color(MUTED),
                );
                ui.add_space(8.0);
                if ui.add(primary_button("BROWSE THE STORE")).clicked() {
                    self.page = Page::Home;
                }
            });
            return;
        }

        let is_verify = verify.is_some();
        let (app_id, name) = match verify {
            Some(job) => (job.app_id, job.name.clone()),
            None => {
                let front = front.as_ref().expect("front present");
                (front.app_id, front.name.clone())
            }
        };
        let active = self
            .download_job
            .as_ref()
            .filter(|job| job.finished.is_none() && job.app_id == app_id);
        let running = active.is_some();
        let verify_finished = verify.and_then(|job| job.finished.clone());
        let paused = !is_verify && !running;
        let error = if paused { self.download_error.clone() } else { None };
        // Progress from the live thread, else the last remembered tick (so a paused bar keeps its
        // position), matched by App ID so it never shows a different game's progress.
        let progress = active.and_then(|job| job.progress.clone()).or_else(|| {
            self.download_last
                .as_ref()
                .filter(|tick| tick.app_id == app_id)
                .cloned()
        });
        let speed = active.map_or(0.0, |job| job.speed_bps);
        let peak = active.map_or(0.0, |job| job.peak_bps);

        let queue_len = self.settings.download_queue.len();
        let mut pause_clicked = false;
        let mut demote_clicked = false;
        let mut resume_clicked = false;
        let mut remove_clicked = false;
        let mut cancel_clicked = false;
        let mut dismiss = false;

        let done = progress.as_ref().map_or(0, |p| p.done_bytes);
        let total = progress.as_ref().map_or(0, |p| p.total_bytes);
        let fraction = if total > 0 {
            (done as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // A wide banner like Steam's Downloads header: the game's hero art fills the card, a solid
        // info panel sits on the right (speed stats on top, the progress bar below), and the game
        // name is set over the art on the left. The panel is opaque with a soft shadow fading into
        // it, so there's no hard seam or stray rounded corner in the middle of the image.
        let width = ui.available_width();
        let banner_h = (width * 0.26).clamp(220.0, 290.0);
        let corner = egui::CornerRadius::same(12);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, banner_h), Sense::hover());
        // The `header.jpg`/`capsule` art matches the banner's art region far better than the very wide
        // `library_hero`, so cover-fitting it fills the region with almost no crop — and no borders.
        let urls = [
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/header.jpg"),
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/capsule_616x353.jpg"),
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/library_hero.jpg"),
        ];
        let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
        // The solid banner base (also the right-hand info panel), then the game art cover-fitted into
        // the left art region — filled edge to edge, no borders.
        ui.painter().rect_filled(rect, corner, SURFACE);
        let panel_w = (width * 0.5).clamp(380.0, 640.0);
        let split_x = rect.right() - panel_w;
        let art_rect = egui::Rect::from_min_max(rect.min, egui::pos2(split_x, rect.bottom()));
        paint_remote_image_cover_multi(
            ui,
            art_rect,
            &refs,
            egui::CornerRadius {
                nw: 12,
                ne: 0,
                sw: 12,
                se: 0,
            },
        );
        {
            let painter = ui.painter().with_clip_rect(art_rect);
            // A bottom band under the name so it stays legible over any art.
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - 58.0),
                    egui::pos2(split_x, rect.bottom()),
                ),
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 12,
                    se: 0,
                },
                Color32::from_rgba_unmultiplied(8, 12, 18, 150),
            );
            painter.text(
                egui::pos2(rect.left() + 22.0, rect.bottom() - 19.0),
                egui::Align2::LEFT_BOTTOM,
                &name,
                FontId::proportional(24.0),
                Color32::WHITE,
            );
        }

        // The info content, laid out inside the solid right-hand panel.
        let content = egui::Rect::from_min_max(
            egui::pos2(split_x + 26.0, rect.top() + 22.0),
            egui::pos2(rect.right() - 26.0, rect.bottom() - 20.0),
        );
        let stage_label = if is_verify {
            if verify_finished.is_some() {
                "Complete"
            } else {
                "Files are being verified…"
            }
        } else if running {
            "Data is downloading…"
        } else if error.is_some() {
            "Paused — download error"
        } else {
            "Paused"
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
            ui.horizontal(|ui| {
                download_mini_stat(ui, "NETWORK", &human_bps(speed), ACCENT_SOFT);
                ui.add_space(24.0);
                download_mini_stat(ui, "PEAK", &human_bps(peak), ACCENT);
                ui.add_space(24.0);
                download_mini_stat(ui, "TOTAL SIZE", &human_bytes(total), AMBER);
            });
            ui.add_space(14.0);
            ui.painter().line_segment(
                [
                    egui::pos2(content.left(), ui.cursor().top()),
                    egui::pos2(content.right(), ui.cursor().top()),
                ],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
            );
            ui.add_space(14.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new(stage_label).size(12.5).strong().color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} / {}", human_bytes(done), human_bytes(total)))
                            .size(12.0)
                            .color(MUTED),
                    );
                });
            });
            ui.add_space(8.0);
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(content.width())
                    .text(format!("{:.0}%", fraction * 100.0)),
            );
            if let Some(p) = &progress {
                if !p.current_file.is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new(tail(&p.current_file, 48)).size(10.5).color(MUTED));
                }
            } else if running {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                    ui.add_space(6.0);
                    ui.label(RichText::new("Preparing…").size(11.0).color(MUTED));
                });
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if is_verify {
                    match &verify_finished {
                        Some(Ok(message)) => {
                            if ui.add(primary_button("DISMISS")).clicked() {
                                dismiss = true;
                            }
                            ui.label(RichText::new(message).size(10.5).color(VERDIGRIS));
                        }
                        Some(Err(message)) => {
                            if ui.add(primary_button("DISMISS")).clicked() {
                                dismiss = true;
                            }
                            ui.label(RichText::new(message).size(10.5).color(DANGER));
                        }
                        None => {
                            if ui.add(ghost_button("CANCEL")).clicked() {
                                cancel_clicked = true;
                            }
                        }
                    }
                } else if running {
                    if ui.add(ghost_button("PAUSE")).clicked() {
                        pause_clicked = true;
                    }
                    if queue_len > 1
                        && ui
                            .add(ghost_button("TO QUEUE"))
                            .on_hover_text("Send this download to the back of the queue and start the next")
                            .clicked()
                    {
                        demote_clicked = true;
                    }
                } else {
                    if ui.add(ghost_button("REMOVE")).clicked() {
                        remove_clicked = true;
                    }
                    if ui.add(primary_button("RESUME")).clicked() {
                        resume_clicked = true;
                    }
                    if let Some(message) = &error {
                        ui.label(RichText::new(message).size(10.5).color(DANGER));
                    }
                }
            });
        });

        // UP NEXT: the queued downloads behind the current one, each removable.
        let upcoming: Vec<QueuedDownload> = self.settings.download_queue.iter().skip(1).cloned().collect();
        let mut remove_queued: Option<u32> = None;
        let mut activate_queued: Option<u32> = None;
        ui.add_space(22.0);
        section_label(ui, &format!("UP NEXT ({})", upcoming.len()));
        ui.add_space(10.0);
        if upcoming.is_empty() {
            panel(ui, |ui| {
                ui.label(RichText::new("No downloads are queued.").size(11.5).color(MUTED));
            });
        } else {
            for item in &upcoming {
                panel(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&item.name).size(12.5).color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(ghost_button("REMOVE")).clicked() {
                                remove_queued = Some(item.app_id);
                            }
                            if ui
                                .add(primary_button("ACTIVATE"))
                                .on_hover_text("Download this now (the current one goes back into the queue)")
                                .clicked()
                            {
                                activate_queued = Some(item.app_id);
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        }

        if pause_clicked {
            self.pause_download();
        }
        if demote_clicked {
            self.demote_current_download();
        }
        if resume_clicked {
            self.start_front_download();
        }
        if remove_clicked {
            self.remove_current_download();
        }
        if let Some(app_id) = activate_queued {
            self.activate_download(app_id);
        }
        if cancel_clicked {
            if let Some(job) = &self.download_job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.status = "Cancelling the verify…".into();
            self.status_error = false;
        }
        if dismiss {
            self.download_job = None;
        }
        if let Some(app_id) = remove_queued {
            self.remove_queued_download(app_id);
        }
    }

}

