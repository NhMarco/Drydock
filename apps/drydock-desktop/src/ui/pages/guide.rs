use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke, Vec2};

use crate::ui::helpers::*;
use crate::ui::theme::*;
use crate::ui::types::*;
use crate::ui::widgets::*;

pub fn guide_tab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = FontId::proportional(13.5);
    let width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width + 28.0, 36.0), Sense::click());
    let hover = ui.ctx().animate_bool(response.id, response.hovered());
    let fill = if active {
        ACCENT_DEEP
    } else {
        lerp_color(Color32::TRANSPARENT, SURFACE_RAISED, hover)
    };
    let text_color = if active {
        Color32::WHITE
    } else {
        lerp_color(MUTED, TEXT, hover)
    };
    let stroke = if active {
        Stroke::new(1.0, ACCENT)
    } else {
        Stroke::NONE
    };

    ui.painter().rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, text_color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, text_color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// One stage of the How It Works guide: a high-contrast index badge with a title and subtitle, then
/// its numbered steps laid out as a connected vertical timeline. `step_number` continues across
/// stages so the steps read 1..N over the whole flow.
pub fn guide_stage(
    ui: &mut egui::Ui,
    index: usize,
    title: &str,
    subtitle: &str,
    steps: &[(&str, &str)],
    step_number: &mut usize,
) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(16)
        .inner_margin(24)
        .show(ui, |ui| {
            // Header: a rounded accent badge carrying the stage index, then the stage title.
            ui.horizontal(|ui| {
                let (badge, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(badge, 10, ACCENT_DEEP);
                painter.rect_stroke(
                    badge,
                    10,
                    Stroke::new(1.0, lerp_color(BORDER, ACCENT, 0.4)),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    badge.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{index:02}"),
                    FontId::proportional(15.0),
                    Color32::WHITE,
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
                    ui.add_space(2.0);
                    ui.label(RichText::new(subtitle).size(13.0).color(ACCENT));
                });
            });
            ui.add_space(18.0);

            // Steps as a timeline: the connecting rail is drawn behind the numbered nodes so it reads
            // as one continuous flow. Node centres are collected during layout, then painted after.
            let rail_width = 36.0;
            let node_radius = 13.0;
            let mut nodes: Vec<f32> = Vec::new();
            let mut rail_x = 0.0_f32;

            ui.vertical(|ui| {
                for (offset, (step_title, detail)) in steps.iter().enumerate() {
                    if offset > 0 {
                        ui.add_space(16.0);
                    }
                    ui.horizontal_top(|ui| {
                        rail_x = ui.cursor().min.x + rail_width / 2.0;
                        ui.add_space(rail_width);
                        let content = ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(RichText::new(*step_title).size(14.0).strong().color(TEXT))
                                    .wrap(),
                            );
                            ui.add_space(2.0);
                            ui.add(egui::Label::new(RichText::new(*detail).size(13.0).color(MUTED)).wrap());
                        });
                        nodes.push(content.response.rect.top() + 9.0);
                    });
                }
            });

            if let (Some(&first), Some(&last)) = (nodes.first(), nodes.last())
                && nodes.len() > 1
            {
                ui.painter().line_segment(
                    [egui::pos2(rail_x, first), egui::pos2(rail_x, last)],
                    Stroke::new(2.0, lerp_color(BORDER, ACCENT_DEEP, 0.6)),
                );
            }
            for &y in &nodes {
                let center = egui::pos2(rail_x, y);
                ui.painter()
                    .circle_filled(center, node_radius, lerp_color(SURFACE, ACCENT_DEEP, 0.4));
                ui.painter().circle_stroke(
                    center,
                    node_radius,
                    Stroke::new(1.5, lerp_color(BORDER, ACCENT, 0.7)),
                );
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    step_number.to_string(),
                    FontId::proportional(12.5),
                    Color32::WHITE,
                );
                *step_number += 1;
            }
        });
}

impl DrydockApp {
    pub fn guide_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            page_heading(ui, &format!("{}  How It Works & Guides", icons::HELP));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Modern segmented switch
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(10)
                    .inner_margin(3)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if guide_tab(
                                ui,
                                &format!("{}  ACTIVATION", icons::CHECK),
                                self.guide_flow == GuideFlow::Activation,
                            )
                            .clicked()
                            {
                                self.guide_flow = GuideFlow::Activation;
                            }
                            if guide_tab(
                                ui,
                                &format!("{}  FIXES", icons::TOOLS),
                                self.guide_flow == GuideFlow::Fixes,
                            )
                            .clicked()
                            {
                                self.guide_flow = GuideFlow::Fixes;
                            }
                        });
                    });
            });
        });
        ui.add_space(18.0);

        // Flow Overview Card
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(24)
            .show(ui, |ui| {
                match self.guide_flow {
                    GuideFlow::Activation => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icons::CHECK).size(18.0).color(ACCENT));
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("GAME ACTIVATION & SIGNED ENTITLEMENTS")
                                    .size(14.0)
                                    .strong()
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Activate Denuvo and DRM-protected games offline. Generate hardware-bound \
                                 request codes, receive cryptographically signed response tokens, and launch \
                                 directly through Steam with update protection.",
                            )
                            .size(13.0)
                            .color(MUTED),
                        );
                    }
                    GuideFlow::Fixes => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icons::TOOLS).size(18.0).color(ACCENT));
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("BUILD-LOCKED GAME FIXES & COMPATIBILITY")
                                    .size(14.0)
                                    .strong()
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Install build-locked fixes, language files, and bypasses with a single click. \
                                 Drydock downloads matched unpack files and locks Steam updates so game updates \
                                 cannot break your setup.",
                            )
                            .size(13.0)
                            .color(MUTED),
                        );
                    }
                }
            });

        ui.add_space(16.0);

        // Each flow is three clear stages so the whole process reads at a glance.
        type GuideStep = (&'static str, &'static str);
        type GuideStage = (&'static str, &'static str, &'static [GuideStep]);
        const ACTIVATION_STAGES: [GuideStage; 3] = [
            (
                "PREPARE",
                "Set up your Steam folder and download the game.",
                &[
                    (
                        "Set your Steam folder",
                        "Open Settings, point Drydock at your Steam folder, and it reads your installed games.",
                    ),
                    (
                        "Install the Steam Service",
                        "In Settings, install the Steam Service — it is required before a game can be added to Steam.",
                    ),
                    (
                        "Download the game fully",
                        "Add a supported game and let Steam finish downloading it completely before activating.",
                    ),
                ],
            ),
            (
                "ACTIVATE",
                "Turn a request code into a signed entitlement.",
                &[
                    (
                        "Generate a request code",
                        "Open Activation, pick the installed game, generate the request code, and copy it.",
                    ),
                    (
                        "Submit it in Discord",
                        "Open a Steam ticket in Discord, choose Activation Code, and paste the request code.",
                    ),
                    (
                        "Enter the response code",
                        "Type the eight-character response code the bot returns, then select Activate.",
                    ),
                ],
            ),
            (
                "PLAY",
                "Verified, protected, and ready to launch.",
                &[
                    (
                        "Automatic verification",
                        "Drydock checks the signature, device, machine, App ID, lifetime, and payload integrity.",
                    ),
                    (
                        "Keep updates paused",
                        "Leave game updates disabled for the entitlement so it keeps working, then launch and play.",
                    ),
                ],
            ),
        ];
        const FIXES_STAGES: [GuideStage; 3] = [
            (
                "PREPARE",
                "Install the game you want to fix.",
                &[
                    (
                        "Set your Steam folder",
                        "Open Settings and point Drydock at your Steam folder so it can see your installed games.",
                    ),
                    (
                        "Install the Steam Service",
                        "In Settings, install the Steam Service — it must be current before a fix can be applied.",
                    ),
                    (
                        "Install the game fully",
                        "Add the game and let Steam finish downloading it completely, so there are files to patch.",
                    ),
                ],
            ),
            (
                "APPLY THE FIX",
                "One click installs everything the fix needs.",
                &[
                    (
                        "Open the Fixes tab",
                        "Pick a game that has a build-locked fix, then open its details page.",
                    ),
                    (
                        "Select Apply Fix",
                        "Drydock installs the matching unlock and downloads the fix files over your game install.",
                    ),
                    (
                        "Let it finish",
                        "Large fixes take a while to download and unpack — keep Drydock open until it reports success.",
                    ),
                ],
            ),
            (
                "PLAY",
                "Fixed, locked to a working build, and ready.",
                &[
                    (
                        "Updates are paused for you",
                        "The fix targets one game build, so Drydock blocks Steam updates to keep an update from breaking it.",
                    ),
                    (
                        "Launch and play",
                        "Start the game from Steam as usual — the fix is already in place.",
                    ),
                ],
            ),
        ];

        let stages: &[GuideStage] = match self.guide_flow {
            GuideFlow::Activation => &ACTIVATION_STAGES,
            GuideFlow::Fixes => &FIXES_STAGES,
        };
        let mut step = 1;
        for (index, (stage, stage_subtitle, steps)) in stages.iter().enumerate() {
            if index > 0 {
                ui.add_space(16.0);
            }
            guide_stage(ui, index + 1, stage, stage_subtitle, steps, &mut step);
        }

        ui.add_space(16.0);

        // Quick Navigation Action Card
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(20)
            .show(ui, |ui| {
                ui.horizontal(|ui| match self.guide_flow {
                    GuideFlow::Activation => {
                        ui.label(
                            RichText::new("Ready to generate an activation code?")
                                .size(13.5)
                                .strong()
                                .color(TEXT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    primary_button(&format!("{}  OPEN ACTIVATION PAGE", icons::ACTIVATION))
                                        .compact(),
                                )
                                .clicked()
                            {
                                self.page = Page::Activation;
                            }
                        });
                    }
                    GuideFlow::Fixes => {
                        ui.label(
                            RichText::new("Ready to explore available fixes and utilities?")
                                .size(13.5)
                                .strong()
                                .color(TEXT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    primary_button(&format!("{}  OPEN TOOLS & UTILITIES", icons::TOOLS))
                                        .compact(),
                                )
                                .clicked()
                            {
                                self.page = Page::Tools;
                            }
                        });
                    }
                });
            });
    }
}
