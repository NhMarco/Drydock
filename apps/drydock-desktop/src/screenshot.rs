//! Debug-only screenshot harness (enabled by the `screenshot` cargo feature).
//!
//! Renders each page of the real [`DrydockApp`] into a PNG using eframe's own
//! framebuffer capture, so the layout can be inspected without a person taking
//! screenshots. Run with:
//!
//! ```text
//! cargo run -p drydock-desktop --features screenshot -- --screenshot <output-dir>
//! ```

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::ui::DrydockApp;

/// Frames rendered per page before capture, giving remote artwork time to load.
const WARMUP_FRAMES: u32 = 220;

/// The details page also waits on the Steam Store fetch, so it needs longer.
fn warmup_for(key: &str) -> u32 {
    if key == "details" { 720 } else { WARMUP_FRAMES }
}

pub fn run(dir: PathBuf) -> eframe::Result {
    let _ = std::fs::create_dir_all(&dir);
    let viewport = egui::ViewportBuilder::default()
        .with_title("Drydock screenshot")
        .with_inner_size([1920.0, 1080.0]);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Drydock screenshot",
        options,
        Box::new(|context| Ok(Box::new(Harness::new(context, dir)))),
    )
}

struct Harness {
    app: DrydockApp,
    dir: PathBuf,
    pages: Vec<&'static str>,
    index: usize,
    warmup: u32,
    requested: bool,
    done: bool,
}

impl Harness {
    fn new(context: &eframe::CreationContext<'_>, dir: PathBuf) -> Self {
        let mut app = DrydockApp::new(context);
        let pages = DrydockApp::SCREENSHOT_PAGES.to_vec();
        app.screenshot_goto(pages[0]);
        let warmup = warmup_for(pages[0]);
        Self {
            app,
            dir,
            pages,
            index: 0,
            warmup,
            requested: false,
            done: false,
        }
    }
}

impl eframe::App for Harness {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.ui(ui, frame);
        if self.done {
            return;
        }
        ctx.request_repaint();

        if self.requested {
            // The reply to last frame's request arrives as an input event this frame.
            let image = ctx.input(|input| {
                input.events.iter().find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let path = self
                    .dir
                    .join(format!("page-{}-{}.png", self.index, self.pages[self.index]));
                save_png(&image, &path);
                eprintln!("screenshot saved: {}", path.display());
                self.requested = false;
                self.index += 1;
                if self.index >= self.pages.len() {
                    self.done = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                self.app.screenshot_goto(self.pages[self.index]);
                self.warmup = warmup_for(self.pages[self.index]);
            }
            return;
        }

        if self.warmup > 0 {
            self.warmup -= 1;
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        self.requested = true;
    }
}

fn save_png(image: &egui::ColorImage, path: &Path) {
    let [width, height] = image.size;
    let raw = image.as_raw().to_vec();
    match image::RgbaImage::from_raw(width as u32, height as u32, raw) {
        Some(buffer) => {
            if let Err(error) = buffer.save(path) {
                eprintln!("failed to save {}: {error}", path.display());
            }
        }
        None => eprintln!("screenshot buffer had an unexpected size"),
    }
}
