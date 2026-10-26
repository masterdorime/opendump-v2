//! opendump native UI.
//!
//! egui drawn straight to the GPU — no Chromium, no WebView2. The engine is the
//! same `opendump-core` the CLI and the Tauri shell use, so classifications and
//! safety behaviour are identical by construction.
//!
//! The scan runs on a worker thread and reports through a channel, because an
//! immediate-mode UI that blocks for three minutes is worse than a spinner.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

use eframe::egui::{self, Color32, RichText};
use serde::Deserialize;

use opendump_core::{
    bytes, compile, default_tokens, find_candidates, list_quarantine, match_rule, purge,
    quarantine, restore, scan, thousands, Candidate, CompiledRule, ExecItem, ExecOptions, Rule,
    Tier,
};

const RULES_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../rules/rules.json"));

// ------------------------------------------------------------------- palette

const BG: Color32 = Color32::from_rgb(0x08, 0x0b, 0x10);
const PANEL: Color32 = Color32::from_rgb(0x0d, 0x13, 0x1b);
const LINE: Color32 = Color32::from_rgb(0x1b, 0x27, 0x34);
const TEXT: Color32 = Color32::from_rgb(0xe7, 0xee, 0xf8);
const MUTED: Color32 = Color32::from_rgb(0x84, 0x97, 0xad);
const DIM: Color32 = Color32::from_rgb(0x5c, 0x6b, 0x7f);
const ACCENT: Color32 = Color32::from_rgb(0x38, 0xbd, 0xf8);
const SAFE: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
const REVIEW: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
const KEEP: Color32 = Color32::from_rgb(0x3b, 0x82, 0xf6);
const LOCKED: Color32 = Color32::from_rgb(0xef, 0x44, 0x44);

// --------------------------------------------------------------------- model

#[derive(Deserialize)]
struct RulesDocument {
    rules: Vec<Rule>,
}

struct ScanView {
    root: String,
    folders: u64,
    files: u64,
    bytes: u64,
    duration: f64,
    candidates: Vec<Candidate>,
}

struct StagedView {
    id: String,
    src: String,
    bytes: u64,
    status: String,
}

enum Message {
    Started,
    Done(ScanView),
    Staged(Vec<StagedView>),
    Action(String),
}

fn base_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("opendump-native"))
        .unwrap_or_else(|_| PathBuf::from(".opendump-native"))
}

fn exec_options() -> ExecOptions {
    ExecOptions {
        quarantine_root: base_dir().join("quarantine"),
        journal_path: base_dir().join("journal.jsonl"),
        batch_id: None,
    }
}

fn rules() -> Vec<CompiledRule> {
    let document: RulesDocument = serde_json::from_str(RULES_JSON).expect("bundled rules parse");
    compile(&document.rules, &default_tokens())
}

fn default_drive() -> String {
    format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()))
}

fn staged_snapshot() -> Vec<StagedView> {
    list_quarantine(&base_dir().join("journal.jsonl"))
        .into_iter()
        .map(|item| StagedView {
            id: item.id,
            src: item.src,
            bytes: item.bytes,
            status: item.status.as_str().to_string(),
        })
        .collect()
}

/// Classify one path exactly as the scanner world would, so the UI cannot ask the
/// executor to remove something the rules do not allow.
fn classify(path: &str, compiled: &[CompiledRule]) -> Result<(ExecItem, String), String> {
    let target = Path::new(path);
    if !target.exists() {
        return Err("does not exist".into());
    }
    let report = scan(target);
    let size = report
        .dirs
        .iter()
        .find(|entry| Path::new(&entry.path) == target)
        .map(|entry| entry.size)
        .unwrap_or(0);

    let parent_has_pkg = target
        .parent()
        .map(|parent| parent.join("package.json").is_file())
        .unwrap_or(false);

    let rule_index = match_rule(path, size, compiled, parent_has_pkg)
        .ok_or_else(|| "no rule classifies this folder".to_string())?;
    let rule = &compiled[rule_index];

    if rule.tier != Tier::SafePurge && rule.tier != Tier::Review {
        return Err(format!("classified {} by {}", rule.tier.as_str(), rule.id));
    }

    Ok((
        ExecItem {
            path: path.to_string(),
            tier: rule.tier,
            rule_id: rule.id.clone(),
            size,
        },
        rule.reason.clone(),
    ))
}

// ----------------------------------------------------------------------- app

struct App {
    sender: Sender<Message>,
    receiver: Receiver<Message>,
    scanning: bool,
    scan_root: String,
    scan: Option<ScanView>,
    picked: std::collections::HashSet<String>,
    staged: Vec<StagedView>,
    confirm_purge: bool,
    toast: Option<(String, Instant)>,
}

impl App {
    fn new(sender: Sender<Message>, receiver: Receiver<Message>) -> Self {
        Self {
            sender,
            receiver,
            scanning: false,
            scan_root: default_drive(),
            scan: None,
            picked: Default::default(),
            staged: staged_snapshot(),
            confirm_purge: false,
            toast: None,
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let sender = self.sender.clone();
        let root = self.scan_root.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let _ = sender.send(Message::Started);
            let started = Instant::now();
            let report = scan(Path::new(&root));
            let compiled = rules();
            let candidates = find_candidates(&report.dirs, &compiled, 1024 * 1024);

            let _ = sender.send(Message::Done(ScanView {
                root: report.root,
                folders: report.dirs.len() as u64,
                files: report.total_files,
                bytes: report.total_bytes,
                duration: started.elapsed().as_secs_f64(),
                candidates,
            }));
            ctx.request_repaint();
        });
    }

    fn quarantine_picked(&mut self, ctx: &egui::Context) {
        let paths: Vec<String> = self.picked.iter().cloned().collect();
        if paths.is_empty() {
            return;
        }
        let sender = self.sender.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let compiled = rules();
            let mut items = Vec::new();
            let mut refused = 0usize;

            for path in &paths {
                match classify(path, &compiled) {
                    Ok((item, _)) => items.push(item),
                    Err(_) => refused += 1,
                }
            }

            let result = quarantine(&items, &exec_options());
            let message = match result {
                Ok(batch) => {
                    let moved = batch
                        .items
                        .iter()
                        .filter(|i| i.status.as_str() == "quarantined")
                        .count();
                    format!(
                        "{} moved to quarantine · {} — nothing deleted{}",
                        moved,
                        bytes(batch.bytes),
                        if refused > 0 {
                            format!(" · {refused} refused")
                        } else {
                            String::new()
                        }
                    )
                }
                Err(error) => format!("quarantine failed: {error}"),
            };

            let _ = sender.send(Message::Action(message));
            let _ = sender.send(Message::Staged(staged_snapshot()));
            ctx.request_repaint();
        });
    }

    fn restore_all(&mut self, ctx: &egui::Context) {
        let ids: Vec<String> = self
            .staged
            .iter()
            .filter(|item| item.status == "quarantined")
            .map(|item| item.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let sender = self.sender.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let message = match restore(&ids, &base_dir().join("journal.jsonl")) {
                Ok(batch) => format!("restored {}", bytes(batch.bytes)),
                Err(error) => format!("restore failed: {error}"),
            };
            let _ = sender.send(Message::Action(message));
            let _ = sender.send(Message::Staged(staged_snapshot()));
            ctx.request_repaint();
        });
    }

    fn purge_all(&mut self, ctx: &egui::Context) {
        let ids: Vec<String> = self
            .staged
            .iter()
            .filter(|item| item.status == "quarantined")
            .map(|item| item.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let sender = self.sender.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let message = match purge(&ids, &base_dir().join("journal.jsonl")) {
                Ok(batch) => format!("{} reclaimed", bytes(batch.bytes)),
                Err(error) => format!("purge failed: {error}"),
            };
            let _ = sender.send(Message::Action(message));
            let _ = sender.send(Message::Staged(staged_snapshot()));
            ctx.request_repaint();
        });
    }

    fn drain(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                Message::Started => self.scanning = true,
                Message::Done(view) => {
                    self.scanning = false;
                    self.picked.clear();
                    self.scan = Some(view);
                }
                Message::Staged(items) => {
                    self.staged = items;
                    self.confirm_purge = false;
                }
                Message::Action(text) => self.toast = Some((text, Instant::now())),
            }
        }
    }
}

// ------------------------------------------------------------------- helpers


fn pretty(path: &str) -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    if !home.is_empty() && path.to_ascii_lowercase().starts_with(&home.to_ascii_lowercase()) {
        return format!("~{}", &path[home.len()..]);
    }
    path.to_string()
}

fn tier_color(tier: Tier) -> Color32 {
    match tier {
        Tier::SafePurge => SAFE,
        Tier::Review => REVIEW,
        Tier::Keep => KEEP,
        Tier::Locked => LOCKED,
    }
}

fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::SafePurge => "SAFE",
        Tier::Review => "REVIEW",
        Tier::Keep => "KEEP",
        Tier::Locked => "LOCKED",
    }
}

// ------------------------------------------------------------------- styling

/// egui ships with its own fonts; matching the other shells means borrowing the
/// system ones. Failure is non-fatal — it just falls back to the defaults.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
        fonts.font_data.insert("segoe".to_owned(), egui::FontData::from_owned(bytes));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".to_owned());
    }
    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\consola.ttf") {
        fonts.font_data.insert("consolas".to_owned(), egui::FontData::from_owned(bytes));
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "consolas".to_owned());
    }

    ctx.set_fonts(fonts);
}

fn install_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(11.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(0.0);

    style.text_styles = [
        (egui::TextStyle::Heading, egui::FontId::new(14.0, egui::FontFamily::Proportional)),
        (egui::TextStyle::Body, egui::FontId::new(12.5, egui::FontFamily::Proportional)),
        (egui::TextStyle::Button, egui::FontId::new(12.0, egui::FontFamily::Proportional)),
        (egui::TextStyle::Small, egui::FontId::new(10.0, egui::FontFamily::Proportional)),
        (egui::TextStyle::Monospace, egui::FontId::new(11.5, egui::FontFamily::Monospace)),
    ]
    .into();

    let visuals = &mut style.visuals;
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = PANEL;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(56, 189, 248, 48);

    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.rounding = egui::Rounding::same(7.0);
        widget.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
        widget.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
        widget.bg_fill = PANEL;
        widget.weak_bg_fill = PANEL;
    }
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x16, 0x20, 0x2c);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x16, 0x20, 0x2c);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(0x2a, 0x3b, 0x4f));
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x16, 0x20, 0x2c);
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, MUTED);

    ctx.set_style(style);
}

// ---------------------------------------------------------------------- view

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.scanning {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = PANEL;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
        visuals.override_text_color = Some(TEXT);
        ctx.set_visuals(visuals);

        egui::TopBottomPanel::top("bar")
            .frame(egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::symmetric(16.0, 12.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("od").color(ACCENT).strong().monospace().size(15.0));
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("opendump").strong().size(13.5));
                        let subtitle = match &self.scan {
                            Some(view) => format!(
                                "{} · {} folders · {} · {:.1}s",
                                view.root,
                                thousands(view.folders),
                                bytes(view.bytes),
                                view.duration
                            ),
                            None => "native core · nothing scanned yet".to_string(),
                        };
                        ui.label(RichText::new(subtitle).size(11.0).color(MUTED).monospace());
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.scanning { "Scanning…" } else { "Scan C:\\" };
                        let button = egui::Button::new(
                            RichText::new(label).color(Color32::from_rgb(0x04, 0x10, 0x19)).strong(),
                        )
                        .fill(ACCENT)
                        .rounding(egui::Rounding::same(7.0));
                        if ui.add_enabled(!self.scanning, button).clicked() {
                            self.start_scan(ctx);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::symmetric(16.0, 12.0)))
            .show(ctx, |ui| {
                self.summary(ui);
                ui.add_space(6.0);
                self.review(ui, ctx);
                ui.add_space(14.0);
                self.quarantine_panel(ui, ctx);
            });

        if let Some((text, at)) = self.toast.clone() {
            if at.elapsed().as_secs() < 6 {
                egui::TopBottomPanel::bottom("toast")
                    .frame(egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::symmetric(16.0, 8.0)))
                    .show(ctx, |ui| {
                        ui.label(RichText::new(text).color(MUTED));
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            } else {
                self.toast = None;
            }
        }
    }
}

impl App {
    fn summary(&self, ui: &mut egui::Ui) {
        let Some(view) = &self.scan else {
            ui.label(RichText::new("Scan the drive to see what can be removed.").color(MUTED));
            return;
        };

        let mut counts = [(0usize, 0u64); 4];
        for candidate in &view.candidates {
            let index = match candidate.tier {
                Tier::SafePurge => 0,
                Tier::Review => 1,
                Tier::Keep => 2,
                Tier::Locked => 3,
            };
            counts[index].0 += 1;
            counts[index].1 += candidate.size;
        }

        egui::Frame::none()
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0_f32, LINE))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    stat(ui, "folders", &thousands(view.folders));
                    stat(ui, "files", &thousands(view.files));
                    stat(ui, "size", &bytes(view.bytes));
                    for (index, tier) in [Tier::SafePurge, Tier::Review, Tier::Keep, Tier::Locked]
                        .into_iter()
                        .enumerate()
                    {
                        let (count, total) = counts[index];
                        let text = if count == 0 {
                            "—".to_string()
                        } else {
                            format!("{} · {}", count, bytes(total))
                        };
                        stat_colored(ui, tier_label(tier), &text, tier_color(tier));
                    }
                });
            });
    }

    fn review(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("REVIEW").size(10.5).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let picked_bytes: u64 = self
                    .scan
                    .as_ref()
                    .map(|view| {
                        view.candidates
                            .iter()
                            .filter(|c| self.picked.contains(&c.path))
                            .map(|c| c.size)
                            .sum()
                    })
                    .unwrap_or(0);

                let enabled = !self.picked.is_empty();
                if ui.add_enabled(enabled, egui::Button::new("Quarantine")).clicked() {
                    self.quarantine_picked(ctx);
                }
                ui.label(
                    RichText::new(format!(
                        "{} selected · {}",
                        self.picked.len(),
                        bytes(picked_bytes)
                    ))
                    .size(11.0)
                    .color(MUTED),
                );
                if ui.button("Select all safe").clicked() {
                    let safe: Vec<String> = self
                        .scan
                        .as_ref()
                        .map(|view| {
                            view.candidates
                                .iter()
                                .filter(|c| c.tier == Tier::SafePurge)
                                .map(|c| c.path.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    self.picked = safe.into_iter().collect();
                }
            });
        });

        ui.add_space(4.0);

        let Some(view) = &self.scan else {
            return;
        };

        egui::ScrollArea::vertical()
            .max_height(360.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for candidate in &view.candidates {
                    let selectable =
                        candidate.tier == Tier::SafePurge || candidate.tier == Tier::Review;

                    egui::Frame::none()
                        .fill(if self.picked.contains(&candidate.path) {
                            Color32::from_rgb(0x0d, 0x18, 0x14)
                        } else {
                            BG
                        })
                        .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let mut checked = self.picked.contains(&candidate.path);
                                let response = ui.add_enabled(
                                    selectable,
                                    egui::Checkbox::without_text(&mut checked),
                                );
                                if response.changed() {
                                    if checked {
                                        self.picked.insert(candidate.path.clone());
                                    } else {
                                        self.picked.remove(&candidate.path);
                                    }
                                }

                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter()
                                    .rect_filled(rect, 2.0, tier_color(candidate.tier));

                                ui.vertical(|ui| {
                                    let mut title =
                                        RichText::new(pretty(&candidate.path)).monospace().size(11.5);
                                    if candidate.blocked {
                                        title = title.color(MUTED);
                                    }
                                    ui.label(title);
                                    ui.label(
                                        RichText::new(&candidate.reason).size(10.5).color(DIM),
                                    );
                                });

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(bytes(candidate.size))
                                                .monospace()
                                                .size(11.5)
                                                .color(MUTED),
                                        );
                                        if candidate.blocked {
                                            ui.label(
                                                RichText::new(format!(
                                                    "holds {} protected",
                                                    bytes(candidate.blocked_bytes)
                                                ))
                                                .size(10.0)
                                                .color(REVIEW),
                                            );
                                        }
                                    },
                                );
                            });
                        });
                }
            });
    }

    fn quarantine_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let active_count = self
            .staged
            .iter()
            .filter(|item| item.status == "quarantined")
            .count();
        let total: u64 = self
            .staged
            .iter()
            .filter(|item| item.status == "quarantined")
            .map(|item| item.bytes)
            .sum();

        ui.horizontal(|ui| {
            ui.label(RichText::new("QUARANTINE").size(10.5).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = active_count > 0;
                let label = if self.confirm_purge {
                    format!("Confirm: delete {}?", bytes(total))
                } else {
                    "Purge all".to_string()
                };
                let fill = if self.confirm_purge {
                    Color32::from_rgb(0x7f, 0x1d, 0x1d)
                } else {
                    Color32::from_rgb(0x3a, 0x14, 0x14)
                };
                if ui
                    .add_enabled(enabled, egui::Button::new(RichText::new(label).color(Color32::from_rgb(0xfc, 0xa5, 0xa5))).fill(fill))
                    .clicked()
                {
                    if self.confirm_purge {
                        self.confirm_purge = false;
                        self.purge_all(ctx);
                    } else {
                        // Deleting staged data is irreversible; a single click is
                        // not enough, so the button arms first.
                        self.confirm_purge = true;
                    }
                }
                if ui.add_enabled(enabled, egui::Button::new("Restore all")).clicked() {
                    self.restore_all(ctx);
                }
                ui.label(
                    RichText::new(if active_count == 0 {
                        "empty".to_string()
                    } else {
                        format!("{} staged · {}", active_count, bytes(total))
                    })
                    .size(11.0)
                    .color(MUTED),
                );
            });
        });

        ui.add_space(4.0);

        if active_count == 0 {
            ui.label(
                RichText::new("Nothing staged. Quarantine is reversible; purge is not.")
                    .size(11.0)
                    .color(DIM),
            );
            return;
        }

        egui::ScrollArea::vertical()
            .max_height(160.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for item in &self.staged {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("■").size(9.0).color(REVIEW));
                        ui.label(RichText::new(pretty(&item.src)).monospace().size(11.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(bytes(item.bytes))
                                    .monospace()
                                    .size(11.0)
                                    .color(MUTED),
                            );
                            ui.label(RichText::new(&item.status).size(10.0).color(DIM));
                        });
                    });
                }
            });
    }
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label.to_uppercase()).size(9.0).color(DIM));
        ui.label(RichText::new(value).monospace().size(13.0).strong());
    });
    ui.add_space(16.0);
}

fn stat_colored(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(9.0).color(color));
        ui.label(RichText::new(value).monospace().size(13.0));
    });
    ui.add_space(16.0);
}


// ------------------------------------------------------------------- selftest

fn selftest() {
    let root = default_drive();
    println!("scanning {root} …");
    let started = Instant::now();
    let report = scan(Path::new(&root));
    let compiled = rules();
    let candidates = find_candidates(&report.dirs, &compiled, 1024 * 1024);

    println!("folders   {}", thousands(report.dirs.len() as u64));
    println!("files     {}", thousands(report.total_files));
    println!("size      {}", bytes(report.total_bytes));
    println!("scan      {:.1}s", started.elapsed().as_secs_f64());

    let mut counts = [(0usize, 0u64); 4];
    for candidate in &candidates {
        let index = match candidate.tier {
            Tier::SafePurge => 0,
            Tier::Review => 1,
            Tier::Keep => 2,
            Tier::Locked => 3,
        };
        counts[index].0 += 1;
        counts[index].1 += candidate.size;
    }
    for (index, label) in ["SAFE", "REVIEW", "KEEP", "LOCKED"].iter().enumerate() {
        println!("  {label:<8} {:>4}  {:>10}", counts[index].0, bytes(counts[index].1));
    }
    println!("staged    {}", staged_snapshot().len());
}

// ----------------------------------------------------------------------- main

fn main() -> eframe::Result<()> {
    if std::env::args().any(|arg| arg == "--selftest") {
        selftest();
        return Ok(());
    }

    let (sender, receiver) = channel();
    let mut app = App::new(sender, receiver);
    let autoscan = std::env::args().any(|arg| arg == "--scan");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("opendump"),
        ..Default::default()
    };

    eframe::run_native(
        "opendump",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            install_style(&cc.egui_ctx);
            if autoscan {
                app.start_scan(&cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}
