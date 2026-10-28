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
    ai, bytes, cache, compile, default_tokens, find_candidates, hardware, inspect_staged,
    list_quarantine, match_rule, parent_of, purge, quarantine, restore, scan, scan_with_progress,
    squarify, thousands, AiTier, Box4, CachedNode, CachedScan, Candidate, ClassifyItem, CompiledRule,
    ExecItem, ExecOptions, Memory, Rule, ScanProgress, StagedDetail, Tier, Verdict,
};

const RULES_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../rules/rules.json"));

// ------------------------------------------------------------------- palette

const BG: Color32 = Color32::from_rgb(0x0a, 0x06, 0x07);
const PANEL: Color32 = Color32::from_rgb(0x15, 0x0c, 0x0e);
const LINE: Color32 = Color32::from_rgb(0x30, 0x1b, 0x1f);
const TEXT: Color32 = Color32::from_rgb(0xf6, 0xec, 0xed);
const MUTED: Color32 = Color32::from_rgb(0xab, 0x94, 0x98);
const DIM: Color32 = Color32::from_rgb(0x77, 0x5e, 0x62);
const ACCENT: Color32 = Color32::from_rgb(0xe0, 0x2b, 0x3c);
/// Text that sits on `ACCENT` fills.
const ON_ACCENT: Color32 = Color32::from_rgb(0xff, 0xf4, 0xf5);
const SAFE: Color32 = Color32::from_rgb(0x2f, 0xc8, 0x6a);
const REVIEW: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
const KEEP: Color32 = Color32::from_rgb(0x4d, 0x8d, 0xf7);
const LOCKED: Color32 = Color32::from_rgb(0xff, 0x2d, 0x55);

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
    Quarantine,
    Ai,
}

/// One rectangle in the nested treemap. Parents are pushed before their
/// children, so painting in order draws the levels on top of each other.
struct Visual {
    rect: egui::Rect,
    path: Option<String>,
    label: String,
    size: u64,
    tier: Option<Tier>,
    depth: usize,
    /// True when nothing was drawn inside this rectangle.
    leaf: bool,
}

/// Layout is rebuilt only when the zoom root or the canvas changes: it is the
/// one part of the map that is expensive, and it is recomputed on mouse move.
struct MapCache {
    root: String,
    rect: egui::Rect,
    visuals: Vec<Visual>,
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
    /// Unix seconds when the walk finished.
    scanned_at: u64,
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
    Progress(ScanProgress),
    AiStatus(Vec<String>),
    AiProgress(usize, usize),
    AiDone(Vec<Verdict>),
    AiError(String),
}

fn base_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("opendump-native"))
        .unwrap_or_else(|_| PathBuf::from(".opendump-native"))
}

fn cache_path() -> PathBuf {
    base_dir().join("scan-cache.bin")
}

/// Invalidate the cache whenever the rules that produced it change.
fn rules_fingerprint() -> u64 {
    cache::rules_hash(RULES_JSON)
}

/// Rebuild the view the window expects from a cache written by another run.
fn view_from_cache(cached: CachedScan) -> ScanView {
    let mut nodes = std::collections::HashMap::new();
    let mut levels: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    for node in cached.nodes {
        if let Some(parent) = parent_of(&node.path) {
            levels.entry(parent).or_default().push(node.path.clone());
        }
        nodes.insert(
            node.path,
            NodeInfo {
                size: node.size,
                files: node.files,
                tier: node.tier,
            },
        );
    }

    for list in levels.values_mut() {
        list.sort_by_key(|path| {
            std::cmp::Reverse(nodes.get(path).map(|node| node.size).unwrap_or(0))
        });
    }

    ScanView {
        root: cached.root,
        folders: cached.folders,
        files: cached.total_files,
        bytes: cached.total_bytes,
        duration: cached.duration_ms as f64 / 1000.0,
        candidates: cached.candidates,
        nodes,
        levels,
        scanned_at: cached.scanned_at,
    }
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
    /// Total and free bytes on the scanned volume.
    space: Option<(u64, u64)>,
    map_cache: Option<MapCache>,
    /// Review filter; `None` means every tier.
    filter: Option<Tier>,
    progress: Option<ScanProgress>,
    scan_started: Option<Instant>,
    ai_endpoint: String,
    ai_model: String,
    ai_models: Vec<String>,
    ai_running: bool,
    ai_progress: (usize, usize),
    ai_results: Vec<Verdict>,
    ai_error: Option<String>,
}

impl App {
    fn new(sender: Sender<Message>, receiver: Receiver<Message>) -> Self {
        let mut app = Self {
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
            space: None,
            map_cache: None,
            filter: None,
            progress: None,
            scan_started: None,
            ai_endpoint: "127.0.0.1:11434".to_string(),
            ai_model: "qwen3.5:4b-q4_K_M".to_string(),
            ai_models: Vec::new(),
            ai_running: false,
            ai_progress: (0, 0),
            ai_results: Vec::new(),
            ai_error: None,
        };

        // Open to the last map when the cache was written by these rules.
        if let Some(cached) = cache::load(&cache_path(), rules_fingerprint()) {
            app.space = hardware::disk_space(&app.scan_root);
            app.scan = Some(view_from_cache(cached));
        }

        app
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.progress = None;
        self.scan_started = Some(Instant::now());
        let sender = self.sender.clone();
        let root = self.scan_root.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let _ = sender.send(Message::Started);
            let started = Instant::now();
            let report = scan_with_progress(Path::new(&root), |progress| {
                let _ = sender.send(Message::Progress(progress));
                ctx.request_repaint();
            });
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

            let scanned_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            let duration_ms = started.elapsed().as_millis() as u64;

            let cached = CachedScan {
                root: report.root.clone(),
                scanned_at,
                duration_ms,
                total_bytes: report.total_bytes,
                total_files: report.total_files,
                folders: report.dirs.len() as u64,
                nodes: nodes
                    .iter()
                    .map(|(path, node)| CachedNode {
                        path: path.clone(),
                        size: node.size,
                        files: node.files,
                        tier: node.tier,
                    })
                    .collect(),
                candidates: candidates.clone(),
            };
            let _ = cache::save(&cache_path(), &cached, rules_fingerprint());

            let _ = sender.send(Message::Done(ScanView {
                root: report.root,
                folders: report.dirs.len() as u64,
                files: report.total_files,
                bytes: report.total_bytes,
                duration: duration_ms as f64 / 1000.0,
                candidates,
                nodes,
                levels,
                scanned_at,
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
                Ok(batch) => {
                    // The sizes on disk just changed; a cached map would be lying.
                    let _ = std::fs::remove_file(cache_path());
                    format!("{} reclaimed", bytes(batch.bytes))
                }
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
                    // A new scan invalidates the previous zoom target and layout.
                    self.zoom = None;
                    self.map_cache = None;
                    self.progress = None;
                    self.scan_started = None;
                }
                Message::Staged(items) => self.staged = items,
                Message::Action(text) => self.toast = Some((text, Instant::now())),
                Message::Memory(memory) => {
                    self.mem = memory;
                    // Cheap syscall, and it has to move when a purge reclaims space.
                    self.space = hardware::disk_space(&self.scan_root);
                }
                Message::Progress(progress) => self.progress = Some(progress),
                Message::AiStatus(models) => self.ai_models = models,
                Message::AiProgress(done, total) => self.ai_progress = (done, total),
                Message::AiDone(verdicts) => {
                    self.ai_running = false;
                    self.ai_results = verdicts;
                }
                Message::AiError(text) => {
                    self.ai_running = false;
                    self.ai_error = Some(text);
                }
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

/// Python-style helper: folders the rules do not understand, reduced to leaves.
fn unclassified_items(view: &ScanView, min_bytes: u64, limit: usize) -> Vec<ClassifyItem> {
    let mut unknown: Vec<(&String, &NodeInfo)> = view
        .nodes
        .iter()
        .filter(|(_, node)| node.tier.is_none() && node.size >= min_bytes)
        .collect();
    unknown.sort_by(|left, right| right.1.size.cmp(&left.1.size));

    let mut out = Vec::new();
    for (path, node) in unknown {
        // A drive root is unknown but useless to ask about.
        if path.trim_end_matches(['\\', '/']).len() <= 2 {
            continue;
        }

        // Descend to a concrete folder: a container of other unknown folders is
        // not the thing to ask about.
        let has_unknown_child = view
            .levels
            .get(path)
            .map(|kids| {
                kids.iter().any(|kid| {
                    view.nodes
                        .get(kid)
                        .map(|child| child.tier.is_none() && child.size >= min_bytes)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if has_unknown_child {
            continue;
        }

        out.push(ClassifyItem {
            path: path.clone(),
            size: node.size,
            files: node.files,
            category: "unclassified".to_string(),
        });
        if out.len() >= limit {
            break;
        }
    }

    out
}

/// Ask Ollama which models exist. Runs once, off the UI thread.
fn spawn_ai_probe(sender: Sender<Message>, endpoint: String) {
    std::thread::spawn(move || {
        let models = ai::models(&endpoint);
        let _ = sender.send(Message::AiStatus(models));
    });
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
    if luma > 100.0 {
        Color32::from_rgb(0x12, 0x08, 0x0a)
    } else {
        TEXT
    }
}

/// Fill for one rectangle: the tier's own colour over the page, getting deeper
/// as you drill. Category is the primary signal — the map is read by verdict —
/// so the hue never shifts away from the tier, only its weight.
fn tile_fill(tier: Option<Tier>, depth: usize) -> Color32 {
    let alpha = 0.34 + (depth.min(6) as f32) * 0.075;

    let (r, g, b) = match tier {
        Some(Tier::SafePurge) => (0x22, 0xc5, 0x5e),
        Some(Tier::Review) => (0xf5, 0x9e, 0x0b),
        Some(Tier::Keep) => (0x3b, 0x82, 0xf6),
        Some(Tier::Locked) => (0xef, 0x44, 0x44),
        None => (0x4e, 0x3f, 0x43),
    };

    let mix = |base: u8, over: u8| (base as f32 * (1.0 - alpha) + over as f32 * alpha) as u8;
    Color32::from_rgb(
        mix(BG.r(), r),
        mix(BG.g(), g),
        mix(BG.b(), b),
    )
}

fn tier_index(tier: Tier) -> usize {
    match tier {
        Tier::SafePurge => 0,
        Tier::Review => 1,
        Tier::Keep => 2,
        Tier::Locked => 3,
    }
}

/// A filter chip: what it selects, how many, and how much.
fn chip(
    ui: &mut egui::Ui,
    label: &str,
    count: usize,
    size: u64,
    active: bool,
    colour: Color32,
) -> bool {
    let text = if size > 0 {
        format!("{label}  {count} · {}", bytes(size))
    } else if count > 0 {
        format!("{label}  {count}")
    } else {
        label.to_string()
    };

    let rich = if active {
        RichText::new(text)
            .size(10.0)
            .color(Color32::from_rgb(0x16, 0x0b, 0x0d))
    } else {
        RichText::new(text).size(10.0).color(MUTED)
    };

    let button = egui::Button::new(rich)
        .fill(if active { colour } else { PANEL })
        .rounding(egui::Rounding::same(11.0))
        .stroke(egui::Stroke::new(
            1.0_f32,
            if active { colour } else { LINE },
        ));

    ui.add(button).clicked()
}

/// The small verdict badge beside a row's path.
fn tier_pill(ui: &mut egui::Ui, tier: Tier) {
    let colour = tier_color(tier);
    egui::Frame::none()
        .fill(colour.gamma_multiply(0.22))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::symmetric(5.0, 1.0))
        .show(ui, |ui| {
            ui.label(RichText::new(tier_label(tier)).size(9.0).color(colour));
        });
}

/// One legend swatch with its label.
fn legend_item(ui: &mut egui::Ui, colour: Color32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, colour);
    ui.label(RichText::new(text).size(9.5).color(MUTED));
    ui.add_space(10.0);
}

/// "scanned 3 min ago" — the phrase v1 had in its header.
fn relative_time(seconds: u64) -> String {
    if seconds == 0 {
        return "never".to_string();
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(seconds);
    let age = now.saturating_sub(seconds);

    match age {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", age / 60),
        3600..=86_399 => format!("{} h ago", age / 3600),
        _ => format!("{} d ago", age / 86_400),
    }
}

fn last_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['\\', '/']);
    // A drive root has no final component; show "C:\" rather than nothing.
    if trimmed.is_empty() || (trimmed.len() <= 2 && trimmed.ends_with(':')) {
        return path;
    }
    trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed)
}

/// Recursively lay a folder out inside `rect`, then its children inside each
/// child, until a rectangle is too small to read.
///
/// This is the difference between a chart and a map: v1 drew every leaf at
/// once, so one glance showed four levels of structure. Returns true when at
/// least one child rectangle was drawn.
fn build_visuals(
    view: &ScanView,
    path: &str,
    size: u64,
    rect: egui::Rect,
    depth: usize,
    inherited: Option<Tier>,
    out: &mut Vec<Visual>,
) -> bool {
    const MAX_DEPTH: usize = 9;
    const MAX_TILES: usize = 12_000;

    if depth >= MAX_DEPTH || out.len() >= MAX_TILES {
        return false;
    }
    if rect.width() < 9.0 || rect.height() < 9.0 {
        return false;
    }

    let Some(children) = view.levels.get(path) else {
        return false;
    };
    if children.is_empty() {
        return false;
    }

    struct Item<'a> {
        path: Option<&'a str>,
        label: &'a str,
        size: u64,
        tier: Option<Tier>,
    }

    let mut sum = 0u64;
    for child in children {
        if let Some(node) = view.nodes.get(child) {
            sum += node.size;
        }
    }
    let direct = size.saturating_sub(sum);

    // Local floor: relative to this folder, not the drive, so deep levels stay
    // populated instead of going blank.
    let floor = (size as f64 * 0.0006).max(1.0) as u64;

    let mut items: Vec<Item> = Vec::new();
    let mut small = 0u64;
    for child in children {
        let Some(node) = view.nodes.get(child) else {
            continue;
        };
        if node.size < floor {
            small += node.size;
            continue;
        }
        items.push(Item {
            path: Some(child.as_str()),
            label: last_component(child),
            size: node.size,
            tier: node.tier,
        });
    }
    if small > 0 {
        items.push(Item {
            path: None,
            label: "smaller",
            size: small,
            tier: None,
        });
    }
    if direct > 0 {
        items.push(Item {
            path: None,
            label: "files",
            size: direct,
            tier: None,
        });
    }
    if items.is_empty() {
        return false;
    }

    items.sort_by(|left, right| right.size.cmp(&left.size));

    let areas: Vec<f64> = items.iter().map(|item| item.size as f64).collect();
    let boxes = squarify(
        &areas,
        Box4::new(
            rect.min.x as f64,
            rect.min.y as f64,
            rect.width() as f64,
            rect.height() as f64,
        ),
    );

    let mut drew = false;
    for (item, area) in items.into_iter().zip(boxes) {
        let tile = egui::Rect::from_min_size(
            egui::pos2(area.x as f32, area.y as f32),
            egui::vec2(area.w as f32, area.h as f32),
        );
        if tile.width() < 2.0 || tile.height() < 2.0 {
            continue;
        }

        let child_path = item.path;
        // A folder inherits the nearest classified verdict above it, so a
        // scratch folder inside a REVIEW tree reads as review rather than as
        // another grey box. Its own tier always wins.
        let tier = item.tier.or(inherited);

        out.push(Visual {
            rect: tile,
            path: child_path.map(str::to_string),
            label: item.label.to_string(),
            size: item.size,
            tier,
            depth,
            leaf: true,
        });
        let index = out.len() - 1;

        if let Some(child_path) = child_path {
            // The top level keeps a strip for its own label; without it the
            // children paint over the name and the map loses its landmarks.
            let inner = if depth == 0 {
                egui::Rect::from_min_max(
                    egui::pos2(tile.min.x + 1.5, tile.min.y + 15.0),
                    egui::pos2(tile.max.x - 1.5, tile.max.y - 1.5),
                )
            } else {
                tile.shrink(1.5)
            };
            if build_visuals(view, child_path, item.size, inner, depth + 1, tier, out) {
                out[index].leaf = false;
            }
        }

        drew = true;
    }

    drew
}

/// Free space on the scanned volume, with the bar v1 had in its header.
fn space_meter(ui: &mut egui::Ui, space: Option<(u64, u64)>) {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let Some((total, free)) = space else {
        return;
    };
    if total == 0 {
        return;
    }

    let used = total.saturating_sub(free);
    let ratio = (used as f64 / total as f64) as f32;
    let colour = if ratio > 0.92 {
        LOCKED
    } else if ratio > 0.82 {
        REVIEW
    } else {
        ACCENT
    };

    // Fixed size on purpose: a nested layout inside the right-to-left header
    // otherwise inherits no width and wraps one character per line.
    ui.allocate_ui_with_layout(
        egui::vec2(140.0, 30.0),
        egui::Layout::top_down(egui::Align::RIGHT),
        |ui| {
            ui.label(
                RichText::new(format!("{:.1} GB free", free as f64 / GB))
                    .size(10.0)
                    .color(TEXT),
            );
            let (rect, _) = ui.allocate_exact_size(egui::vec2(140.0, 4.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 2.0, Color32::from_rgb(0x2c, 0x18, 0x1c));
            let mut filled = rect;
            filled.set_width(rect.width() * ratio.clamp(0.0, 1.0));
            ui.painter().rect_filled(filled, 2.0, colour);
        },
    );
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
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(224, 43, 60, 56);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);

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
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x24, 0x12, 0x16);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x24, 0x12, 0x16);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(0x53, 0x24, 0x2c));
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x24, 0x12, 0x16);
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
            spawn_ai_probe(self.sender.clone(), self.ai_endpoint.clone());
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
                                "{} · {} folders · {} · scanned {} ({:.0}s)",
                                view.root,
                                thousands(view.folders),
                                bytes(view.bytes),
                                relative_time(view.scanned_at),
                                view.duration
                            ),
                            None => "native core · nothing scanned yet".to_string(),
                        };
                        ui.label(RichText::new(subtitle).size(11.0).color(MUTED).monospace());
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.scanning {
                            "Scanning…"
                        } else if self.scan.is_some() {
                            "Rescan"
                        } else {
                            "Scan C:\\"
                        };
                        let button = egui::Button::new(
                            RichText::new(label).color(ON_ACCENT).strong(),
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
                        ui.add_space(18.0);
                        space_meter(ui, self.space);
                    });
                });

                if self.scanning {
                    self.scan_progress_bar(ui);
                }
            });

        if self.pane == Pane::Map && self.scan.is_some() {
            egui::SidePanel::right("inspector")
                .exact_width(304.0)
                .resizable(false)
                .frame(
                    egui::Frame::none()
                        .fill(BG)
                        .inner_margin(egui::Margin::symmetric(14.0, 12.0)),
                )
                .show(ctx, |ui| self.inspector(ui));
        }

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
                    Pane::Quarantine => self.quarantine_panel(ui, ctx),
                    Pane::Ai => self.ai_pane(ui, ctx),
                }
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
        let Some(view) = &self.scan else {
            ui.label(RichText::new("Scan the drive to see what can be removed.").color(MUTED));
            return;
        };

        // Counts and bytes per tier, for the filter chips.
        let mut counts = [(0usize, 0u64); 4];
        for candidate in &view.candidates {
            let slot = &mut counts[tier_index(candidate.tier)];
            slot.0 += 1;
            slot.1 += candidate.size;
        }
        let total_count: usize = counts.iter().map(|slot| slot.0).sum();
        let total_bytes: u64 = counts.iter().map(|slot| slot.1).sum();

        let mut pick: Option<Option<Tier>> = None;

        ui.scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(9.0, 4.0);
            ui.horizontal_wrapped(|ui| {
                if chip(ui, "All", total_count, total_bytes, self.filter.is_none(), ACCENT) {
                    pick = Some(None);
                }
                for tier in [Tier::SafePurge, Tier::Review, Tier::Keep, Tier::Locked] {
                    let (count, size) = counts[tier_index(tier)];
                    if chip(
                        ui,
                        tier_label(tier),
                        count,
                        size,
                        self.filter == Some(tier),
                        tier_color(tier),
                    ) {
                        pick = Some(Some(tier));
                    }
                }
            });
        });

        if let Some(choice) = pick {
            self.filter = choice;
        }

        ui.add_space(8.0);

        let mut visible: Vec<&Candidate> = view
            .candidates
            .iter()
            .filter(|candidate| self.filter.map_or(true, |tier| candidate.tier == tier))
            .collect();

        // Actionable tiers first: a review list that opens on fifty protected
        // folders is a list of things you cannot do anything about.
        visible.sort_by(|left, right| {
            tier_index(left.tier)
                .cmp(&tier_index(right.tier))
                .then(right.size.cmp(&left.size))
        });

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} rows", visible.len()))
                    .size(10.0)
                    .color(DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Select all safe").clicked() {
                    let safe: Vec<String> = view
                        .candidates
                        .iter()
                        .filter(|candidate| candidate.tier == Tier::SafePurge)
                        .map(|candidate| candidate.path.clone())
                        .collect();
                    self.picked = safe.into_iter().collect();
                }
            });
        });

        ui.add_space(4.0);

        let list_height = (ui.available_height() - 92.0).max(200.0);

        egui::ScrollArea::vertical()
            .max_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for candidate in &visible {
                    let selectable =
                        candidate.tier == Tier::SafePurge || candidate.tier == Tier::Review;

                    egui::Frame::none()
                        .fill(if self.picked.contains(&candidate.path) {
                            Color32::from_rgb(0x22, 0x12, 0x15)
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
                                    ui.horizontal(|ui| {
                                        let mut title = RichText::new(pretty(&candidate.path))
                                            .monospace()
                                            .size(11.5);
                                        if candidate.blocked {
                                            title = title.color(MUTED);
                                        }
                                        ui.label(title);
                                        tier_pill(ui, candidate.tier);
                                    });
                                    ui.label(
                                        RichText::new(&candidate.reason)
                                            .size(10.5)
                                            .color(DIM),
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

        ui.add_space(8.0);

        let picked_bytes: u64 = view
            .candidates
            .iter()
            .filter(|candidate| self.picked.contains(&candidate.path))
            .map(|candidate| candidate.size)
            .sum();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} selected · {} reclaimable",
                    self.picked.len(),
                    bytes(picked_bytes)
                ))
                .size(10.5)
                .color(MUTED),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = !self.picked.is_empty();
                let button = egui::Button::new(
                    RichText::new("Quarantine selected").color(ON_ACCENT),
                )
                .fill(if enabled {
                    ACCENT
                } else {
                    Color32::from_rgb(0x3a, 0x1a, 0x1e)
                });
                if ui.add_enabled(enabled, button).clicked() {
                    self.quarantine_picked(ctx);
                }
                if ui.button("Clear").clicked() {
                    self.picked.clear();
                }
            });
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
            .max_height((ui.available_height() - 96.0).max(140.0))
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

    /// Live progress while the walk runs.
    ///
    /// The denominator is the volume's used bytes, so the percentage is work
    /// actually completed rather than an animated lie. It stalls while a huge
    /// folder is read and jumps at the end, which is the truth of the walk.
    fn scan_progress_bar(&self, ui: &mut egui::Ui) {
        let used = self
            .space
            .map(|(total, free)| total.saturating_sub(free))
            .unwrap_or(0);

        let seen = self.progress.as_ref().map(|p| p.bytes).unwrap_or(0);
        let ratio = if used > 0 {
            (seen as f32 / used as f32).clamp(0.0, 0.99)
        } else {
            0.0
        };

        let elapsed = self
            .scan_started
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let eta = if ratio > 0.03 {
            elapsed / ratio as f64 * (1.0 - ratio as f64)
        } else {
            0.0
        };

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{:.0}%", ratio * 100.0))
                    .size(10.5)
                    .color(TEXT)
                    .monospace(),
            );
            if let Some(progress) = &self.progress {
                ui.label(
                    RichText::new(format!(
                        "{} folders · {} files · {}",
                        thousands(progress.folders),
                        thousands(progress.files),
                        bytes(progress.bytes)
                    ))
                    .size(10.0)
                    .color(MUTED)
                    .monospace(),
                );
            }
            if eta > 1.0 {
                ui.label(
                    RichText::new(format!("~{eta:.0}s left"))
                        .size(10.0)
                        .color(DIM)
                        .monospace(),
                );
            }
        });

        let (track, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 6.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(track, 3.0, Color32::from_rgb(0x2c, 0x18, 0x1c));
        let mut filled = track;
        filled.set_width(track.width() * ratio);
        painter.rect_filled(filled, 3.0, ACCENT);

        if let Some(progress) = &self.progress {
            ui.label(
                RichText::new(clamp_label(&progress.current, 110))
                    .size(9.0)
                    .color(DIM)
                    .monospace(),
            );
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let actionable = self
            .scan
            .as_ref()
            .map(|view| view.candidates.len())
            .unwrap_or(0);
        let held = self
            .staged
            .iter()
            .filter(|item| item.status == "quarantined")
            .count();

        let tabs = [
            (Pane::Map, "MAP", 0usize),
            (Pane::Review, "REVIEW", actionable),
            (Pane::Quarantine, "QUARANTINE", held),
            (Pane::Ai, "AI", self.ai_results.len()),
        ];

        ui.horizontal(|ui| {
            for (pane, label, count) in tabs {
                let active = self.pane == pane;
                let text = if count > 0 {
                    format!("{label}  {count}")
                } else {
                    label.to_string()
                };
                let rich = RichText::new(text)
                    .size(10.5)
                    .color(if active { TEXT } else { MUTED });
                if ui.selectable_label(active, rich).clicked() {
                    self.pane = pane;
                }
            }
        });
    }

    /// The treemap: area is bytes, colour is safety, click zooms.
    fn map(&mut self, ui: &mut egui::Ui) {
        if self.scan.is_none() {
            ui.label(RichText::new("Scan the drive to draw the map.").color(MUTED));
            return;
        }

        let root = self.scan.as_ref().unwrap().root.clone();
        let current = self.zoom.clone().unwrap_or_else(|| root.clone());

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
        });
        ui.add_space(4.0);

        let height = (ui.available_height() - 26.0).max(200.0);
        let (canvas, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click(),
        );

        self.ensure_visuals(&current, canvas);

        let cache = self.map_cache.as_ref().unwrap();
        let view = self.scan.as_ref().unwrap();
        let current_size = view
            .nodes
            .get(&current)
            .map(|node| node.size)
            .unwrap_or(view.bytes);

        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, egui::Rounding::same(6.0), PANEL);

        for visual in &cache.visuals {
            if visual.rect.width() < 1.5 || visual.rect.height() < 1.5 {
                continue;
            }

            let fill = tile_fill(visual.tier, visual.depth);
            painter.rect_filled(visual.rect, 0.0, fill);
            // A hairline in the page colour separates one level from the next.
            painter.rect_stroke(visual.rect, 0.0, egui::Stroke::new(1.0_f32, BG));

            let roomy = visual.rect.width() > 54.0 && visual.rect.height() > 20.0;
            if roomy && (visual.leaf || visual.depth == 0) {
                let max = (visual.rect.width() / 6.4) as usize;
                // Aggregate tiles are labelled quietly: "files" repeated at
                // every level would read as loudly as a folder name.
                let label_colour = if visual.path.is_none() {
                    on_fill(fill).gamma_multiply(0.5)
                } else {
                    on_fill(fill)
                };
                painter.text(
                    visual.rect.min + egui::vec2(6.0, 5.0),
                    egui::Align2::LEFT_TOP,
                    clamp_label(&visual.label, max),
                    egui::FontId::monospace(10.5),
                    label_colour,
                );
                if visual.rect.height() > 38.0 && visual.depth > 0 {
                    painter.text(
                        visual.rect.min + egui::vec2(6.0, 21.0),
                        egui::Align2::LEFT_TOP,
                        bytes(visual.size),
                        egui::FontId::monospace(10.0),
                        on_fill(fill).gamma_multiply(0.72),
                    );
                }
            }
        }

        if let Some(pos) = response.hover_pos() {
            // The deepest rectangle wins, as in v1: the leaf is what you meant.
            if let Some(visual) = cache
                .visuals
                .iter()
                .filter(|visual| visual.rect.contains(pos))
                .max_by_key(|visual| visual.depth)
            {
                let share = visual.size as f64 / current_size.max(1) as f64 * 100.0;
                let share_text = if share >= 0.1 {
                    format!("{share:.1}%")
                } else if share > 0.0 {
                    "<0.1%".to_string()
                } else {
                    "0%".to_string()
                };
                let matched = visual.path.as_ref().and_then(|path| {
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

                        let title = visual
                            .path
                            .clone()
                            .unwrap_or_else(|| visual.label.clone());
                        ui.label(RichText::new(title).monospace().size(11.5));
                        ui.label(
                            RichText::new(format!(
                                "{} · {} of this folder",
                                bytes(visual.size),
                                share_text
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
                        } else if visual.path.is_some() {
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
                // The deepest tile may be an aggregate ("files", "smaller"),
                // which has no folder of its own. Zoom the folder that owns it
                // instead of doing nothing.
                if let Some(visual) = cache
                    .visuals
                    .iter()
                    .filter(|visual| visual.rect.contains(pos) && visual.path.is_some())
                    .max_by_key(|visual| visual.depth)
                {
                    if let Some(path) = &visual.path {
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

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for (tier, label) in [
                (Tier::SafePurge, "Safe"),
                (Tier::Review, "Review"),
                (Tier::Keep, "Keep"),
                (Tier::Locked, "Locked"),
            ] {
                legend_item(ui, tier_color(tier), label);
            }
            legend_item(ui, tile_fill(None, 7), "Unclassified");
        });
    }

    /// Rebuild the nested layout only when the root or the canvas changed.
    fn ensure_visuals(&mut self, current: &str, canvas: egui::Rect) {
        let stale = match &self.map_cache {
            Some(cache) => cache.root != current || cache.rect != canvas,
            None => true,
        };
        if !stale {
            return;
        }

        let view = self.scan.as_ref().unwrap();
        let size = view
            .nodes
            .get(current)
            .map(|node| node.size)
            .unwrap_or(view.bytes);

        let mut visuals = Vec::new();
        build_visuals(view, current, size, canvas, 0, None, &mut visuals);

        self.map_cache = Some(MapCache {
            root: current.to_string(),
            rect: canvas,
            visuals,
        });
    }

    /// Local AI: the model's opinion on folders the rules do not understand.
    ///
    /// Nothing here can authorise deletion — the ceiling is REVIEW, and the
    /// results are shown as suggestions with their confidence attached.
    fn ai_pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let reachable = !self.ai_models.is_empty();
        let ram_free = self.mem.ram_free;
        let gate_ok = ram_free >= 1_200 * 1024 * 1024;

        ui.horizontal(|ui| {
            ui.label(RichText::new("LOCAL AI").size(10.5).color(MUTED));
            if reachable {
                ui.label(
                    RichText::new(format!("ollama · {} models", self.ai_models.len()))
                        .size(10.0)
                        .color(SAFE),
                );
            } else {
                ui.label(
                    RichText::new("ollama not reachable at 127.0.0.1:11434")
                        .size(10.0)
                        .color(REVIEW),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{:.1} GB free", ram_free as f64 / 1024f64.powi(3)))
                        .size(10.0)
                        .color(if gate_ok { MUTED } else { LOCKED }),
                );
            });
        });

        ui.add_space(8.0);

        if !reachable {
            ui.label(
                RichText::new("Start Ollama, then reopen this view.")
                    .size(11.0)
                    .color(DIM),
            );
            return;
        }

        ui.label(RichText::new("MODEL").size(9.5).color(DIM));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(9.0, 4.0);
            for model in self.ai_models.clone() {
                let active = model == self.ai_model;
                if chip(ui, &model, 0, 0, active, ACCENT) {
                    self.ai_model = model;
                }
            }
        });

        ui.add_space(10.0);

        let pending = self
            .scan
            .as_ref()
            .map(|view| unclassified_items(view, 64 * 1024 * 1024, 16).len())
            .unwrap_or(0);

        ui.horizontal(|ui| {
            let label = RichText::new(if self.ai_running {
                "Classifying…".to_string()
            } else {
                format!("Classify {pending} unclassified folders")
            })
            .color(ON_ACCENT);

            let button = egui::Button::new(label).fill(if gate_ok && pending > 0 {
                ACCENT
            } else {
                Color32::from_rgb(0x3a, 0x1a, 0x1e)
            });

            if ui
                .add_enabled(gate_ok && !self.ai_running && pending > 0, button)
                .clicked()
            {
                self.run_ai(ctx);
            }

            if !gate_ok {
                ui.label(
                    RichText::new("needs 1.2 GB free RAM")
                        .size(10.0)
                        .color(LOCKED),
                );
            } else if self.ai_progress.1 > 0 && self.ai_running {
                ui.label(
                    RichText::new(format!(
                        "{} of {}",
                        self.ai_progress.0, self.ai_progress.1
                    ))
                    .size(10.0)
                    .color(MUTED)
                    .monospace(),
                );
            }
        });

        if let Some(error) = &self.ai_error {
            ui.add_space(6.0);
            ui.label(RichText::new(error).size(10.5).color(LOCKED));
        }

        ui.add_space(10.0);
        ui.label(
            RichText::new(format!("SUGGESTIONS · {}", self.ai_results.len()))
                .size(9.5)
                .color(DIM),
        );
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 20.0).max(160.0))
            .show(ui, |ui| {
                if self.ai_results.is_empty() {
                    ui.label(
                        RichText::new("Nothing yet. The model only ever sees folders the rules cannot classify.")
                            .size(10.5)
                            .color(DIM),
                    );
                }

                for verdict in &self.ai_results {
                    ui.horizontal(|ui| {
                        let colour = match verdict.tier {
                            AiTier::Keep => KEEP,
                            AiTier::Review => REVIEW,
                        };
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, colour);

                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(pretty(&verdict.path))
                                        .monospace()
                                        .size(11.0),
                                );
                                egui::Frame::none()
                                    .fill(colour.gamma_multiply(0.22))
                                    .rounding(egui::Rounding::same(5.0))
                                    .inner_margin(egui::Margin::symmetric(5.0, 1.0))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(verdict.tier.as_str())
                                                .size(9.0)
                                                .color(colour),
                                        );
                                    });
                            });
                            ui.label(
                                RichText::new(&verdict.reason).size(10.0).color(DIM),
                            );
                        });

                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{:.0}%",
                                        verdict.confidence * 100.0
                                    ))
                                    .monospace()
                                    .size(10.0)
                                    .color(MUTED),
                                );
                            },
                        );
                    });
                    ui.add_space(4.0);
                }
            });
    }

    fn run_ai(&mut self, ctx: &egui::Context) {
        if self.ai_running {
            return;
        }
        let Some(view) = self.scan.as_ref() else {
            return;
        };

        let items = unclassified_items(view, 64 * 1024 * 1024, 16);
        if items.is_empty() {
            self.ai_error = Some("No unclassified folders above 64 MB.".to_string());
            return;
        }

        self.ai_running = true;
        self.ai_error = None;
        self.ai_results.clear();
        self.ai_progress = (0, items.len());

        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let endpoint = self.ai_endpoint.clone();
        let model = self.ai_model.clone();
        let total = items.len();

        std::thread::spawn(move || {
            let mut collected = Vec::new();
            for (index, batch) in items.chunks(8).enumerate() {
                match ai::classify(&endpoint, &model, batch) {
                    Ok(verdicts) => collected.extend(verdicts),
                    Err(error) => {
                        let _ = sender.send(Message::AiError(error));
                        ctx.request_repaint();
                        return;
                    }
                }
                let _ = sender.send(Message::AiProgress(
                    ((index + 1) * 8).min(total),
                    total,
                ));
                ctx.request_repaint();
            }

            let _ = sender.send(Message::AiDone(collected));
            ctx.request_repaint();
        });
    }

    /// The right-hand panel: what is inside the folder you are looking at.
    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(view) = self.scan.as_ref() else {
            return;
        };

        let root = view.root.clone();
        let current = self.zoom.clone().unwrap_or_else(|| root.clone());
        let node = view.nodes.get(&current);
        let size = node.map(|node| node.size).unwrap_or(view.bytes);
        let files = node.map(|node| node.files).unwrap_or(view.files);
        let share = size as f64 / view.bytes.max(1) as f64 * 100.0;

        let mut go: Option<String> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(last_component(&current)).strong().size(13.5));
                    if self.zoom.is_some() && ui.button("Up").clicked() {
                        go = parent_of(&current);
                    }
                });
                ui.label(RichText::new(&current).monospace().size(9.5).color(DIM));

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    stat(ui, "size", &bytes(size));
                    stat(ui, "of drive", &format!("{share:.1}%"));
                    stat(ui, "files", &thousands(files));
                });

                ui.add_space(12.0);
                ui.label(RichText::new("CONTENTS").size(9.5).color(DIM));
                ui.add_space(4.0);

                match view.levels.get(&current) {
                    Some(children) if !children.is_empty() => {
                        for child in children.iter().take(200) {
                            let Some(node) = view.nodes.get(child) else {
                                continue;
                            };
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(7.0, 7.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    node.tier.map(tier_color).unwrap_or(DIM),
                                );
                                let label =
                                    RichText::new(last_component(child)).size(11.0).color(TEXT);
                                if ui.selectable_label(false, label).clicked() {
                                    go = Some(child.clone());
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(bytes(node.size))
                                                .monospace()
                                                .size(10.5)
                                                .color(MUTED),
                                        );
                                    },
                                );
                            });
                        }
                        if children.len() > 200 {
                            ui.label(
                                RichText::new(format!("+{} more", children.len() - 200))
                                    .size(10.0)
                                    .color(DIM),
                            );
                        }
                    }
                    _ => {
                        ui.label(
                            RichText::new("Nothing above the display threshold.")
                                .size(10.5)
                                .color(DIM),
                        );
                    }
                }

                ui.add_space(12.0);
                if ui.button("Reveal in Explorer").clicked() {
                    let _ = std::process::Command::new("explorer").arg(&current).spawn();
                }
            });

        if let Some(path) = go {
            self.zoom = if path == root { None } else { Some(path) };
        }
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
