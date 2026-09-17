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



/// Formats a byte-per-second rate as a compact human string (e.g. `9.9 MB/s`).

pub fn download_mini_stat(ui: &mut egui::Ui, caption: &str, value: &str, accent: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(RichText::new(caption).size(14.0).strong().color(accent));
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
        let installed_dir = installed_directory(&self.settings, &self.manifests, app_id);
        let games_directory = self.games_directory();
        let limits = JobLimits {
            connections: match self.settings.max_download_connections {
                0 => Settings::DEFAULT_DOWNLOAD_CONNECTIONS,
                n => n.clamp(1, 32),
            } as usize,
            max_bps: match self.settings.max_download_mbps {
                0 => None,
                mbps => Some(u64::from(mbps) * 1024 * 1024),
            },
            verify_threads: self.settings.verify_threads,
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
                games_directory,
                limits,
                &thread_cancel,
                &sender,
            );
            let _ = sender.send(DownloadUpdate::Finished(result));
        });
        self.download_job = Some(DownloadJob {
            install_root: None,
            verified: None,
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
                    Ok(DownloadUpdate::Installed(root)) => job.install_root = Some(root),
                    Ok(DownloadUpdate::Verified(complete)) => job.verified = Some(complete),
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
                    .map(|job| (job.app_id, job.name.clone(), job.install_root.clone()));
                self.download_job = None; // the download thread has ended
                match result {
                    Ok(_) => {
                        // Completed: drop it from the queue by App ID (robust to reordering), persist,
                        // register it in the Drydock library, and resume the next one.
                        if let Some((id, name, root)) = finished_game {
                            if let Some(install_root) = root {
                                drydock_core::download_queue::register_completed(
                                    &mut self.settings,
                                    id,
                                    &name,
                                    &install_root,
                                );
                            } else {
                                download_queue::remove_completed(&mut self.settings.download_queue, id);
                            }
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
}

fn format_eta(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let mins = secs / 60;
        let s = secs % 60;
        format!("{mins}m {s:02}s")
    } else {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        format!("{hours}h {mins:02}m")
    }
}

fn modern_progress_bar(ui: &mut egui::Ui, fraction: f32, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let corner = egui::CornerRadius::same((height / 2.0) as u8);
    // Background track
    ui.painter().rect_filled(
        rect,
        corner,
        Color32::from_rgba_unmultiplied(6, 14, 24, 220),
    );
    ui.painter().rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, Color32::from_white_alpha(25)),
        egui::StrokeKind::Inside,
    );
    // Filled bar
    let fill_w = (rect.width() * fraction.clamp(0.0, 1.0)).max(0.0);
    if fill_w > 0.0 {
        let fill_rect = egui::Rect::from_min_size(rect.min, Vec2::new(fill_w, height));
        ui.painter().rect_filled(
            fill_rect,
            corner,
            ACCENT,
        );
        if fill_w > 8.0 {
            let glow_rect = egui::Rect::from_min_size(
                egui::pos2(fill_rect.right() - 5.0, fill_rect.top()),
                Vec2::new(5.0, height),
            );
            ui.painter().rect_filled(
                glow_rect,
                corner,
                Color32::from_white_alpha(140),
            );
        }
    }
}

impl DrydockApp {
    pub fn downloads_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);

        // Header with live telemetry pills on the right
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  Downloads", icons::DOWNLOAD));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let queue_len = self.settings.download_queue.len();
                if queue_len > 0 {
                    status_pill(ui, &format!("{} IN QUEUE", queue_len), MUTED);
                }
                if let Some(job) = self.download_job.as_ref().filter(|j| j.finished.is_none()) {
                    if job.speed_bps > 0.0 {
                        status_pill(ui, &format!("⚡ {}", human_bps(job.speed_bps)), ACCENT);
                    }
                }
            });
        });
        ui.add_space(16.0);

        // The banner shows a running/finished verify, or the current download (queue front) whether
        // running, paused, or stopped by an error. With neither, there is nothing to download.
        let verify = self
            .download_job
            .as_ref()
            .filter(|job| job.kind == DownloadKind::Verify);
        let front = self.settings.download_queue.first().cloned();
        if verify.is_none() && front.is_none() {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(16)
                .inner_margin(36)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new(icons::DOWNLOAD).size(42.0).color(ACCENT));
                        ui.add_space(14.0);
                        ui.label(RichText::new("No Active Downloads").size(22.0).strong().color(TEXT));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Games you download or verify will appear here with live transfer speeds and progress.")
                                .size(14.0)
                                .color(MUTED),
                        );
                        ui.add_space(22.0);

                        // Centered button row
                        let btn_w = 160.0;
                        let gap = 12.0;
                        let total_w = btn_w * 2.0 + gap;
                        let side_space = ((ui.available_width() - total_w) / 2.0).max(0.0);
                        ui.horizontal(|ui| {
                            ui.add_space(side_space);
                            ui.spacing_mut().item_spacing = Vec2::new(gap, 0.0);
                            if ui.add(primary_button(&format!("{}  BROWSE STORE", icons::STORE)).min_size(Vec2::new(btn_w, 40.0))).clicked() {
                                self.page = Page::Home;
                            }
                            if ui.add(ghost_button(&format!("{}  VIEW LIBRARY", icons::LIBRARY)).min_size(Vec2::new(btn_w, 40.0))).clicked() {
                                self.page = Page::Library;
                            }
                        });
                        ui.add_space(12.0);
                    });
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

        // ── Active Download Hero Card ──────────────────────────────────────────
        let width = ui.available_width();
        let banner_h = 240.0;
        let corner = egui::CornerRadius::same(16);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, banner_h), Sense::hover());

        // Background container
        ui.painter().rect_filled(rect, corner, SURFACE);
        ui.painter().rect_stroke(rect, corner, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);

        // Left Cover Art with smooth gradient fade into SURFACE
        let art_w = (width * 0.36).clamp(240.0, 360.0);
        let art_rect = egui::Rect::from_min_size(rect.min, Vec2::new(art_w, banner_h));
        let urls = [
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/header.jpg"),
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/capsule_616x353.jpg"),
            format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/library_hero.jpg"),
        ];
        let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
        paint_remote_image_cover_multi(
            ui,
            art_rect,
            &refs,
            egui::CornerRadius {
                nw: 16,
                ne: 0,
                sw: 16,
                se: 0,
            },
        );

        // Horizontal gradient fade (right 45% of art_rect blends into SURFACE)
        {
            let fade_w = art_w * 0.45;
            let fade_start = art_rect.right() - fade_w;
            let strips = 20usize;
            let painter = ui.painter().with_clip_rect(art_rect);
            for i in 0..strips {
                let t = (i + 1) as f32 / strips as f32;
                let x0 = fade_start + fade_w * (i as f32 / strips as f32);
                let x1 = fade_start + fade_w * t;
                let a = (t.powf(1.5) * 255.0) as u8;
                let band = egui::Rect::from_min_max(
                    egui::pos2(x0, rect.top()),
                    egui::pos2(x1, rect.bottom()),
                );
                painter.rect_filled(band, 0, Color32::from_rgba_unmultiplied(13, 27, 40, a));
            }
        }

        // Left Art Overlays: App ID badge + Title + Status
        {
            let painter = ui.painter().with_clip_rect(art_rect);

            // App ID Badge (top-left)
            let app_badge_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 16.0, rect.top() + 16.0),
                Vec2::new(76.0, 22.0),
            );
            painter.rect_filled(app_badge_rect, egui::CornerRadius::same(5), Color32::from_black_alpha(170));
            painter.rect_stroke(app_badge_rect, egui::CornerRadius::same(5), Stroke::new(1.0, Color32::from_white_alpha(35)), egui::StrokeKind::Inside);
            painter.text(
                app_badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("APP {app_id}"),
                FontId::proportional(11.0),
                Color32::from_white_alpha(160),
            );

            // Bottom gradient for text readability
            let bottom_fade_h = 100.0;
            for i in 0..16usize {
                let t = (i + 1) as f32 / 16.0;
                let y0 = rect.bottom() - bottom_fade_h * (1.0 - (i as f32 / 16.0));
                let y1 = rect.bottom() - bottom_fade_h * (1.0 - t);
                let a = (t.powf(1.6) * 230.0) as u8;
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(rect.left(), y0), egui::pos2(art_rect.right(), y1)),
                    if i == 15 { egui::CornerRadius { sw: 16, nw: 0, ne: 0, se: 0 } } else { egui::CornerRadius::ZERO },
                    Color32::from_rgba_unmultiplied(6, 14, 24, a),
                );
            }

            // Status dot + label
            let (status_text, status_color) = if is_verify {
                if verify_finished.is_some() {
                    ("VERIFY COMPLETE", VERDIGRIS)
                } else {
                    ("VERIFYING FILES", ACCENT)
                }
            } else if running {
                ("DOWNLOADING", VERDIGRIS)
            } else if error.is_some() {
                ("DOWNLOAD ERROR", DANGER)
            } else {
                ("PAUSED", AMBER)
            };

            let status_y = rect.bottom() - 20.0;
            painter.circle_filled(egui::pos2(rect.left() + 20.0, status_y), 4.0, status_color);
            painter.text(
                egui::pos2(rect.left() + 30.0, status_y),
                egui::Align2::LEFT_CENTER,
                status_text,
                FontId::proportional(12.0),
                status_color,
            );

            // Game Name above status
            let title_text = ellipsize(&name, 26);
            painter.text(
                egui::pos2(rect.left() + 18.0, status_y - 14.0),
                egui::Align2::LEFT_BOTTOM,
                title_text,
                FontId::proportional(22.0),
                TEXT,
            );
        }

        // Right Info Content: Telemetry Bento + Custom Progress Bar + Controls
        let info_rect = egui::Rect::from_min_max(
            egui::pos2(art_rect.right() + 18.0, rect.top() + 18.0),
            egui::pos2(rect.right() - 20.0, rect.bottom() - 18.0),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(info_rect), |ui| {
            // 1. Live Telemetry Strip (4 stats)
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                let eta_str = if running && speed > 1024.0 && total > done {
                    let rem = total.saturating_sub(done);
                    format!("~{}", format_eta((rem as f64 / speed) as u64))
                } else if running {
                    "Calculating…".to_string()
                } else if is_verify {
                    "—".to_string()
                } else {
                    "Paused".to_string()
                };

                let speed_str = if running { human_bps(speed) } else { "0 B/s".to_string() };
                let peak_str = if peak > 0.0 { human_bps(peak) } else { "—".to_string() };
                let progress_str = if total > 0 {
                    format!("{} / {}", human_bytes(done), human_bytes(total))
                } else {
                    "—".to_string()
                };

                download_mini_stat(ui, "SPEED", &speed_str, ACCENT);
                download_mini_stat(ui, "PEAK", &peak_str, ACCENT_SOFT);
                download_mini_stat(ui, "TRANSFERRED", &progress_str, TEXT);
                download_mini_stat(ui, "TIME LEFT", &eta_str, AMBER);
            });

            ui.add_space(14.0);
            ui.painter().line_segment(
                [
                    egui::pos2(info_rect.left(), ui.cursor().min.y),
                    egui::pos2(info_rect.right(), ui.cursor().min.y),
                ],
                Stroke::new(1.0, Color32::from_white_alpha(20)),
            );
            ui.add_space(14.0);

            // 2. Activity status & Percentage
            ui.horizontal(|ui| {
                let pct = format!("{:.1}%", fraction * 100.0);
                ui.label(RichText::new(pct).size(15.0).strong().color(ACCENT_SOFT));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(p) = &progress {
                        if !p.current_file.is_empty() {
                            ui.label(RichText::new(tail(&p.current_file, 42)).size(12.5).color(MUTED));
                            ui.label(RichText::new(icons::FOLDER).size(12.0).color(MUTED));
                        }
                    } else if running {
                        ui.label(RichText::new("Initializing download…").size(12.5).color(MUTED));
                        ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                    }
                });
            });
            ui.add_space(8.0);

            // 3. Custom Neon Progress Bar
            modern_progress_bar(ui, fraction, info_rect.width(), 10.0);

            // 4. Action Command Bar (bottom-right aligned)
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                if is_verify {
                    match &verify_finished {
                        Some(Ok(message)) => {
                            if ui.add(primary_button("DISMISS").compact()).clicked() {
                                dismiss = true;
                            }
                            ui.label(RichText::new(message).size(13.0).color(VERDIGRIS));
                        }
                        Some(Err(message)) => {
                            if ui.add(primary_button("DISMISS").compact()).clicked() {
                                dismiss = true;
                            }
                            ui.label(RichText::new(message).size(13.0).color(DANGER));
                        }
                        None => {
                            if ui.add(ghost_button(&format!("{}  CANCEL", icons::CLOSE)).compact()).clicked() {
                                cancel_clicked = true;
                            }
                        }
                    }
                } else if running {
                    if ui.add(ghost_button(&format!("{}  PAUSE", icons::CLOSE)).compact()).clicked() {
                        pause_clicked = true;
                    }
                    if queue_len > 1
                        && ui
                            .add(ghost_button(&format!("{}  TO QUEUE", icons::CHEVRON_RIGHT)).compact())
                            .on_hover_text("Send this download to the back of the queue and start the next")
                            .clicked()
                    {
                        demote_clicked = true;
                    }
                } else {
                    if ui.add(ghost_button(&format!("{}  REMOVE", icons::CLOSE)).compact()).clicked() {
                        remove_clicked = true;
                    }
                    if ui.add(success_button(&format!("{}  RESUME", icons::PLAY)).compact()).clicked() {
                        resume_clicked = true;
                    }
                    if let Some(message) = &error {
                        ui.label(RichText::new(message).size(13.0).color(DANGER));
                    }
                }
            });
        });

        // ── UP NEXT: Queued downloads behind the current one ──────────────────
        let upcoming: Vec<QueuedDownload> = self.settings.download_queue.iter().skip(1).cloned().collect();
        let mut remove_queued: Option<u32> = None;
        let mut activate_queued: Option<u32> = None;

        ui.add_space(28.0);
        ui.horizontal(|ui| {
            section_label(ui, &format!("UP NEXT ({})", upcoming.len()));
            if !upcoming.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("Downloads start automatically in order").size(12.5).color(MUTED));
                });
            }
        });
        ui.add_space(10.0);

        if upcoming.is_empty() {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(12)
                .inner_margin(20)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::CHECK).size(16.0).color(MUTED));
                        ui.add_space(6.0);
                        ui.label(RichText::new("No other downloads are queued.").size(14.0).color(MUTED));
                    });
                });
        } else {
            for (idx, item) in upcoming.iter().enumerate() {
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(12.0, 0.0);

                            // 1. Order badge (#1, #2...)
                            let badge_size = Vec2::new(32.0, 32.0);
                            let (badge_rect, _) = ui.allocate_exact_size(badge_size, Sense::hover());
                            ui.painter().rect_filled(badge_rect, egui::CornerRadius::same(6), SURFACE_RAISED);
                            ui.painter().rect_stroke(badge_rect, egui::CornerRadius::same(6), Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
                            ui.painter().text(
                                badge_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                format!("#{}", idx + 1),
                                FontId::monospace(13.0),
                                ACCENT_SOFT,
                            );

                            // 2. Mini Game Header Thumbnail
                            let thumb_size = Vec2::new(86.0, 40.0);
                            let (thumb_rect, _) = ui.allocate_exact_size(thumb_size, Sense::hover());
                            let thumb_urls = [
                                format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg", item.app_id),
                                format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{}/capsule_231x87.jpg", item.app_id),
                            ];
                            let thumb_refs: Vec<&str> = thumb_urls.iter().map(String::as_str).collect();
                            paint_remote_image_cover_multi(ui, thumb_rect, &thumb_refs, egui::CornerRadius::same(6));

                            // 3. Info (Title + Subtitle)
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(&item.name).size(15.0).strong().color(TEXT));
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(RichText::new(format!("APP {}", item.app_id)).size(12.0).color(MUTED));
                                    ui.label(RichText::new("·").size(12.0).color(MUTED));
                                    ui.label(RichText::new("Waiting in queue").size(12.0).color(AMBER));
                                });
                            });

                            // 4. Action buttons (right-aligned)
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                                if ui
                                    .add(ghost_button(&format!("{}  REMOVE", icons::CLOSE)).compact())
                                    .on_hover_text("Remove this game from the download queue")
                                    .clicked()
                                {
                                    remove_queued = Some(item.app_id);
                                }
                                if ui
                                    .add(primary_button(&format!("{}  DOWNLOAD NOW", icons::DOWNLOAD)).compact())
                                    .on_hover_text("Start downloading this game immediately (moves to front)")
                                    .clicked()
                                {
                                    activate_queued = Some(item.app_id);
                                }
                            });
                        });
                    });
                ui.add_space(8.0);
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
