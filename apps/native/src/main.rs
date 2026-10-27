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
    bytes, compile, default_tokens, find_candidates, hardware, inspect_staged, list_quarantine,
    match_rule, parent_of, purge, quarantine, restore, scan, squarify, thousands, Box4, Candidate,
    CompiledRule, ExecItem, ExecOptions, Memory, Rule, StagedDetail, Tier,
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

/// One directory as the map sees it: recursive size, file count, and the tier a
/// rule gave it (if any). Deliberately smaller than `DirEntry` — the map keeps
/// tens of thousands of these.
struct NodeInfo {
    size: u64,
    files: u64,
    tier: Option<Tier>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Map,
    Review,
}

/// A laid-out rectangle in the treemap.
struct Tile {
    path: Option<String>,
    label: String,
    size: u64,
    tier: Option<Tier>,
    rect: egui::Rect,
}

struct ScanView {
    root: String,
    folders: u64,
    files: u64,
    bytes: u64,
    duration: f64,
    candidates: Vec<Candidate>,
    nodes: std::collections::HashMap<String, NodeInfo>,
    /// parent path -> child paths, largest first.
    levels: std::collections::HashMap<String, Vec<String>>,
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
    Memory(Memory),
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

struct ConfirmState {
    ids: Vec<String>,
    details: Vec<StagedDetail>,
    bytes: u64,
    files: u64,
    acknowledged: bool,
}

struct App {
    sender: Sender<Message>,
    receiver: Receiver<Message>,
    scanning: bool,
    scan_root: String,
    scan: Option<ScanView>,
    picked: std::collections::HashSet<String>,
    staged: Vec<StagedView>,
    confirm: Option<ConfirmState>,
    toast: Option<(String, Instant)>,
    pane: Pane,
    /// Current treemap root; `None` means the scan root.
    zoom: Option<String>,
    mem: Memory,
    probe_started: bool,
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
            confirm: None,
            toast: None,
            pane: Pane::Map,
            zoom: None,
            mem: Memory::default(),
            probe_started: false,
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

            // The scanner returns a flat list; the map needs a tree. Prune before
            // grouping: a full C:\ is ~368k directories, and a 2 MB floor keeps
            // the retained set small without losing any parent's total, because
            // a parent is always at least as large as each of its children.
            let floor = (report.total_bytes / 200_000).max(2 * 1024 * 1024);
            let mut nodes: std::collections::HashMap<String, NodeInfo> =
                std::collections::HashMap::new();
            let mut levels: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();

            for dir in report.dirs.iter().filter(|dir| dir.size >= floor) {
                nodes.insert(
                    dir.path.clone(),
                    NodeInfo {
                        size: dir.size,
                        files: dir.files,
                        tier: None,
                    },
                );
                if let Some(parent) = parent_of(&dir.path) {
                    levels.entry(parent).or_default().push(dir.path.clone());
                }
            }

            // The root survives the floor on any drive that has data.
            nodes.entry(report.root.clone()).or_insert(NodeInfo {
                size: report.total_bytes,
                files: report.total_files,
                tier: None,
            });

            for candidate in &candidates {
                if let Some(node) = nodes.get_mut(&candidate.path) {
                    node.tier = Some(candidate.tier);
                }
            }

            for list in levels.values_mut() {
                list.sort_by_key(|path| {
                    std::cmp::Reverse(nodes.get(path).map(|node| node.size).unwrap_or(0))
                });
            }

            let _ = sender.send(Message::Done(ScanView {
                root: report.root,
                folders: report.dirs.len() as u64,
                files: report.total_files,
                bytes: report.total_bytes,
                duration: started.elapsed().as_secs_f64(),
                candidates,
                nodes,
                levels,
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

    fn purge_ids(&mut self, ids: Vec<String>, ctx: &egui::Context) {
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
                    // A new scan invalidates the previous zoom target.
                    self.zoom = None;
                }
                Message::Staged(items) => self.staged = items,
                Message::Action(text) => self.toast = Some((text, Instant::now())),
                Message::Memory(memory) => self.mem = memory,
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

/// Poll memory off the UI thread.
///
/// RAM is one syscall; VRAM is a subprocess, so it refreshes every fourth poll
/// (about six seconds) instead of every frame.
fn spawn_probe(sender: Sender<Message>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut gpu = hardware::query_gpu();
        let mut tick = 0u32;

        loop {
            let mut memory = hardware::memory();
            memory.gpu = gpu.clone();

            if sender.send(Message::Memory(memory)).is_err() {
                return; // window closed
            }
            ctx.request_repaint();

            tick = tick.wrapping_add(1);
            if tick % 4 == 0 {
                gpu = hardware::query_gpu();
            }
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
    });
}

/// One memory readout. An unmeasurable value prints an em dash: a blank is
/// honest, a guessed number is not.
fn metric(ui: &mut egui::Ui, label: &str, values: Option<(u64, u64)>) {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let (text, color) = match values {
        Some((free, total)) if total > 0 => {
            let used = total.saturating_sub(free);
            let headroom = free as f32 / total as f32;
            let color = if headroom < 0.10 {
                LOCKED
            } else if headroom < 0.25 {
                REVIEW
            } else {
                MUTED
            };
            (
                format!("{:.1}/{:.1} GB", used as f64 / GB, total as f64 / GB),
                color,
            )
        }
        _ => ("—".to_string(), DIM),
    };

    // One widget, not a nested `vertical`: inside a right-to-left layout a
    // nested block inherits almost no width and wraps to one character per line.
    ui.label(
        RichText::new(format!("{label} {text}"))
            .size(10.0)
            .color(color)
            .monospace(),
    );
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Trim a label to roughly fit a tile, keeping it readable with an ellipsis.
fn clamp_label(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Choose readable text for an arbitrary fill, by luminance.
fn on_fill(fill: Color32) -> Color32 {
    let luma = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    if luma > 140.0 {
        Color32::from_rgb(0x06, 0x0a, 0x10)
    } else {
        TEXT
    }
}

/// Large areas in the vivid tier colour would be unreadable, so fills are dark
/// tints and the vivid colour is kept for the tile's top edge.
fn tile_tint(tier: Tier) -> Color32 {
    match tier {
        Tier::SafePurge => Color32::from_rgb(0x11, 0x3d, 0x27),
        Tier::Review => Color32::from_rgb(0x3d, 0x2c, 0x0c),
        Tier::Keep => Color32::from_rgb(0x10, 0x24, 0x45),
        Tier::Locked => Color32::from_rgb(0x3a, 0x14, 0x14),
    }
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

        // The probe needs a context to request repaints, which does not exist
        // until the first frame.
        if !self.probe_started {
            self.probe_started = true;
            spawn_probe(self.sender.clone(), ctx.clone());
        }

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

                        ui.add_space(18.0);
                        metric(
                            ui,
                            "vram",
                            self.mem
                                .gpu
                                .as_ref()
                                .map(|gpu| (gpu.vram_free, gpu.vram_total)),
                        );
                        ui.add_space(14.0);
                        metric(ui, "ram", Some((self.mem.ram_free, self.mem.ram_total)));
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::symmetric(16.0, 12.0)))
            .show(ctx, |ui| {
                self.summary(ui);
                ui.add_space(10.0);
                self.tabs(ui);
                ui.add_space(6.0);
                match self.pane {
                    Pane::Map => self.map(ui),
                    Pane::Review => self.review(ui, ctx),
                }
                ui.add_space(14.0);
                self.quarantine_panel(ui, ctx);
            });

        self.confirm_modal(ctx);

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
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(
                            RichText::new("Purge all").color(Color32::from_rgb(0xfc, 0xa5, 0xa5)),
                        )
                        .fill(Color32::from_rgb(0x3a, 0x14, 0x14)),
                    )
                    .clicked()
                {
                    // Never delete without showing what is inside.
                    let staged = list_quarantine(&base_dir().join("journal.jsonl"));
                    let active: Vec<_> = staged
                        .into_iter()
                        .filter(|item| item.status.as_str() == "quarantined")
                        .collect();
                    if !active.is_empty() {
                        let details = inspect_staged(&active);
                        self.confirm = Some(ConfirmState {
                            ids: active.iter().map(|item| item.id.clone()).collect(),
                            files: details.iter().map(|detail| detail.file_count).sum(),
                            bytes: active.iter().map(|item| item.bytes).sum(),
                            details,
                            acknowledged: false,
                        });
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

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (pane, label) in [(Pane::Map, "MAP"), (Pane::Review, "REVIEW")] {
                let active = self.pane == pane;
                let text = RichText::new(label)
                    .size(10.5)
                    .color(if active { TEXT } else { MUTED });
                if ui.selectable_label(active, text).clicked() {
                    self.pane = pane;
                }
            }
        });
    }

    /// The treemap: area is bytes, colour is safety, click zooms.
    fn map(&mut self, ui: &mut egui::Ui) {
        let Some(view) = self.scan.as_ref() else {
            ui.label(RichText::new("Scan the drive to draw the map.").color(MUTED));
            return;
        };

        let root = view.root.clone();
        let current = self.zoom.clone().unwrap_or_else(|| root.clone());
        let node = view.nodes.get(&current);
        let current_size = node.map(|n| n.size).unwrap_or(view.bytes);
        let current_files = node.map(|n| n.files).unwrap_or(view.files);

        let mut zoom_out = false;
        let mut zoom_to: Option<String> = None;

        ui.horizontal(|ui| {
            if self.zoom.is_some() && ui.button("‹ up").clicked() {
                zoom_out = true;
            }
            ui.label(
                RichText::new(pretty(&current))
                    .monospace()
                    .size(11.0)
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · {} files",
                        bytes(current_size),
                        thousands(current_files)
                    ))
                    .size(10.5)
                    .color(DIM),
                );
            });
        });

        // Leave room for the quarantine strip below without needing a scroll area.
        let height = (ui.available_height() - 150.0).max(220.0);
        let (canvas, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click(),
        );

        let tiles = self.layout_tiles(view, &current, current_size, canvas);
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, egui::Rounding::same(6.0), PANEL);

        for tile in &tiles {
            if tile.rect.width() < 2.0 || tile.rect.height() < 2.0 {
                continue;
            }

            let fill = tile
                .tier
                .map(tile_tint)
                .unwrap_or(Color32::from_rgb(0x1c, 0x27, 0x35));
            let rounding = egui::Rounding::same(3.0);
            painter.rect_filled(tile.rect, rounding, fill);

            // A vivid top edge carries the tier without making large areas loud.
            if let Some(tier) = tile.tier {
                let edge = egui::Rect::from_min_size(
                    tile.rect.min,
                    egui::vec2(tile.rect.width(), 3.0),
                );
                painter.rect_filled(edge, rounding, tier_color(tier));
            }

            if tile.rect.width() > 54.0 && tile.rect.height() > 20.0 {
                let max = (tile.rect.width() / 6.4) as usize;
                painter.text(
                    tile.rect.min + egui::vec2(6.0, 5.0),
                    egui::Align2::LEFT_TOP,
                    clamp_label(&tile.label, max),
                    egui::FontId::monospace(10.5),
                    on_fill(fill),
                );
            }
            if tile.rect.width() > 54.0 && tile.rect.height() > 38.0 {
                painter.text(
                    tile.rect.min + egui::vec2(6.0, 21.0),
                    egui::Align2::LEFT_TOP,
                    bytes(tile.size),
                    egui::FontId::monospace(10.0),
                    on_fill(fill).gamma_multiply(0.72),
                );
            }
        }

        if let Some(pos) = response.hover_pos() {
            if let Some(tile) = tiles.iter().find(|tile| tile.rect.contains(pos)) {
                let share = tile.size as f64 / current_size.max(1) as f64 * 100.0;
                let matched = tile.path.as_ref().and_then(|path| {
                    view.candidates
                        .iter()
                        .find(|candidate| &candidate.path == path)
                        .map(|candidate| (candidate.tier, candidate.reason.clone()))
                });

                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    egui::Id::new("treemap-tip"),
                    |ui| {
                        // Without a width floor egui can squeeze this to one
                        // character per line when the pointer is at the edge.
                        ui.set_min_width(240.0);
                        ui.set_max_width(420.0);

                        // Full path here, not `pretty`: "~" alone is correct but
                        // reads like a rendering failure when the tile _is_ the profile.
                        let title = tile
                            .path
                            .clone()
                            .unwrap_or_else(|| tile.label.clone());
                        ui.label(RichText::new(title).monospace().size(11.5));
                        ui.label(
                            RichText::new(format!(
                                "{} · {:.1}% of this folder",
                                bytes(tile.size),
                                share
                            ))
                            .size(10.5)
                            .color(MUTED),
                        );
                        if let Some((tier, reason)) = matched {
                            ui.label(
                                RichText::new(tier_label(tier))
                                    .size(10.5)
                                    .color(tier_color(tier)),
                            );
                            ui.label(RichText::new(reason).size(10.5).color(DIM));
                        } else if tile.path.is_some() {
                            ui.label(
                                RichText::new("no rule matched · not a candidate")
                                    .size(10.5)
                                    .color(DIM),
                            );
                        }
                    },
                );
            }
        }

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some(tile) = tiles.iter().find(|tile| tile.rect.contains(pos)) {
                    if let Some(path) = &tile.path {
                        // Only zoom where there is something below to show.
                        if view.levels.contains_key(path) {
                            zoom_to = Some(path.clone());
                        }
                    }
                }
            }
        }

        if zoom_out {
            self.zoom = match parent_of(&current) {
                Some(parent) if parent != root => Some(parent),
                _ => None,
            };
        }
        if let Some(path) = zoom_to {
            self.zoom = Some(path);
        }
    }

    /// One level of the tree, as rectangles.
    ///
    /// Children smaller than a sliver of the canvas are folded into a single
    /// tile rather than drawn as invisible hairlines, and bytes that live
    /// directly in this folder (not in any child) get their own tile — otherwise
    /// the level would not add up to the folder's total.
    fn layout_tiles(
        &self,
        view: &ScanView,
        current: &str,
        current_size: u64,
        canvas: egui::Rect,
    ) -> Vec<Tile> {
        let floor = (current_size as f64 * 0.0015) as u64;
        let mut entries: Vec<Tile> = Vec::new();
        let mut small = 0u64;
        let mut child_sum = 0u64;

        if let Some(children) = view.levels.get(current) {
            for path in children {
                let Some(node) = view.nodes.get(path) else {
                    continue;
                };
                child_sum += node.size;
                if node.size < floor {
                    small += node.size;
                    continue;
                }
                entries.push(Tile {
                    path: Some(path.clone()),
                    label: file_name(path),
                    size: node.size,
                    tier: node.tier,
                    rect: egui::Rect::NOTHING,
                });
            }
        }

        if small > 0 {
            entries.push(Tile {
                path: None,
                label: "smaller folders".to_string(),
                size: small,
                tier: None,
                rect: egui::Rect::NOTHING,
            });
        }

        let direct = current_size.saturating_sub(child_sum);
        if direct > 0 {
            entries.push(Tile {
                path: None,
                label: "files in this folder".to_string(),
                size: direct,
                tier: None,
                rect: egui::Rect::NOTHING,
            });
        }

        let areas: Vec<f64> = entries.iter().map(|tile| tile.size as f64).collect();
        let boxes = squarify(
            &areas,
            Box4::new(
                canvas.min.x as f64,
                canvas.min.y as f64,
                canvas.width() as f64,
                canvas.height() as f64,
            ),
        );

        for (tile, area) in entries.iter_mut().zip(boxes) {
            let rect = egui::Rect::from_min_size(
                egui::pos2(area.x as f32, area.y as f32),
                egui::vec2(area.w as f32, area.h as f32),
            );
            // A hairline gutter keeps neighbouring tiles legible.
            tile.rect = if rect.width() > 4.0 && rect.height() > 4.0 {
                rect.shrink(1.0)
            } else {
                rect
            };
        }

        entries
    }

    /// The last screen before data stops existing.
    ///
    /// It answers "what am I destroying?" rather than "how much?" — folder
    /// paths, real filenames, a file count, and a tick you must make before the
    /// confirm button enables.
    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.confirm.take() else {
            return;
        };

        let mut cancel = false;
        let mut confirmed = false;

        let screen = ctx.screen_rect();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("purge-dim"),
        ))
        .rect_filled(screen, 0.0, Color32::from_black_alpha(190));

        // Swallow clicks so the panels behind the dialog cannot be operated.
        egui::Area::new(egui::Id::new("purge-block"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.set_min_size(screen.size());
                ui.allocate_response(screen.size(), egui::Sense::click());
            });

        egui::Window::new("Permanently delete")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        Color32::from_rgb(0x5a, 0x24, 0x24),
                    )),
            )
            .show(ctx, |ui| {
                ui.set_width(560.0);
                ui.label(
                    RichText::new(format!(
                        "Delete {} folder{} · {} permanently",
                        state.ids.len(),
                        if state.ids.len() == 1 { "" } else { "s" },
                        bytes(state.bytes)
                    ))
                    .strong()
                    .size(14.0),
                );
                ui.label(
                    RichText::new(
                        "There is no undo after this. Quarantine could be restored; this cannot.",
                    )
                    .size(11.0)
                    .color(Color32::from_rgb(0xfc, 0xca, 0xca)),
                );
                ui.add_space(8.0);

                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for detail in &state.details {
                        ui.label(RichText::new(pretty(&detail.src)).monospace().size(11.0));
                        ui.label(
                            RichText::new(if detail.truncated {
                                format!("{}+ files", thousands(detail.file_count))
                            } else {
                                format!("{} files", thousands(detail.file_count))
                            })
                            .size(10.0)
                            .color(DIM),
                        );
                        for (name, size) in &detail.top_files {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(name).monospace().size(10.5).color(MUTED));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(bytes(*size))
                                                .monospace()
                                                .size(10.5)
                                                .color(DIM),
                                        );
                                    },
                                );
                            });
                        }
                        ui.add_space(8.0);
                    }
                });

                ui.add_space(6.0);
                ui.checkbox(
                    &mut state.acknowledged,
                    format!(
                        "I understand these {} files will be destroyed",
                        thousands(state.files)
                    ),
                );

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    let button = egui::Button::new(
                        RichText::new(format!("Permanently delete {}", bytes(state.bytes)))
                            .color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(0x7f, 0x1d, 0x1d));
                    if ui.add_enabled(state.acknowledged, button).clicked() {
                        confirmed = true;
                    }
                });
            });

        if confirmed {
            self.purge_ids(state.ids.clone(), ctx);
        } else if !cancel {
            self.confirm = Some(state);
        }
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

/// Hardware readout. `--probe` prints this without scanning 400 GB first.
fn print_memory() {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let memory = hardware::memory();
    println!(
        "ram       {:.1} GB free of {:.1} GB",
        memory.ram_free as f64 / GB,
        memory.ram_total as f64 / GB
    );

    match hardware::query_gpu() {
        Some(gpu) => println!(
            "gpu       {} · {:.1} GB free of {:.1} GB",
            gpu.name,
            gpu.vram_free as f64 / GB,
            gpu.vram_total as f64 / GB
        ),
        None => println!("gpu       no NVIDIA adapter could be queried (vram left unknown)"),
    }
}

fn selftest() {
    let root = default_drive();
    print_memory();
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
    if std::env::args().any(|arg| arg == "--probe") {
        print_memory();
        return Ok(());
    }

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
