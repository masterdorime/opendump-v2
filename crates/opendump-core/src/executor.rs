//! The only module with write power.
//!
//! Everything else in this crate reads. This is where a folder moves, and it is
//! built around the same three rules as the TypeScript implementation:
//!
//! * **Intent is journaled before the filesystem changes.** A crash leaves a
//!   `begin` record without a result, which [`list_quarantine`] reports as
//!   `Interrupted` instead of losing track of a folder.
//! * **Only an explicit list is acted on.** Nothing here discovers work.
//! * **Refusals are loud.** `KEEP`/`LOCKED`, drive roots, symlinks, cross-volume
//!   moves and paths inside the quarantine are rejected, never silently coerced.
//!
//! A move is a rename, so the destination must share the source's volume. The
//! caller is responsible for choosing a root per volume.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::rules::normalize;
use crate::types::Tier;

#[derive(Debug, Clone)]
pub struct ExecItem {
    pub path: String,
    pub tier: Tier,
    pub rule_id: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub quarantine_root: PathBuf,
    pub journal_path: PathBuf,
    /// Overridden by tests so a batch is reproducible.
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecStatus {
    Quarantined,
    Restored,
    Purged,
    Skipped,
    Refused,
}

impl ExecStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecStatus::Quarantined => "quarantined",
            ExecStatus::Restored => "restored",
            ExecStatus::Purged => "purged",
            ExecStatus::Skipped => "skipped",
            ExecStatus::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub path: String,
    pub dst: Option<String>,
    pub status: ExecStatus,
    pub reason: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct BatchResult {
    pub batch: String,
    pub items: Vec<ExecResult>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineStatus {
    Quarantined,
    Restored,
    Purged,
    Missing,
    Interrupted,
}

impl QuarantineStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineStatus::Quarantined => "quarantined",
            QuarantineStatus::Restored => "restored",
            QuarantineStatus::Purged => "purged",
            QuarantineStatus::Missing => "missing",
            QuarantineStatus::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct QuarantineItem {
    pub id: String,
    pub src: String,
    pub dst: String,
    pub bytes: u64,
    pub batch: String,
    pub ts_ms: u64,
    pub status: QuarantineStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Planned {
    id: String,
    src: String,
    dst: String,
    bytes: u64,
}

/// One journal line. Deliberately flat and permissive so an older or newer
/// record never breaks replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    op: String,
    batch: String,
    ts_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    src: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dst: Option<String>,
    #[serde(default)]
    bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    items: Vec<Planned>,
}

/// Move approved folders into quarantine. Returns once every item has a result.
pub fn quarantine(items: &[ExecItem], options: &ExecOptions) -> std::io::Result<BatchResult> {
    let batch = options
        .batch_id
        .clone()
        .unwrap_or_else(|| format!("{}", now_nanos().unwrap_or(0)));

    let planned: Vec<Planned> = items
        .iter()
        .enumerate()
        .map(|(index, item)| Planned {
            id: format!("{}-{index}", now_nanos().unwrap_or(0)),
            src: item.path.clone(),
            dst: staged_path_for(&options.quarantine_root, &batch, &item.path)
                .to_string_lossy()
                .to_string(),
            bytes: item.size,
        })
        .collect();

    // Journal the intent before touching anything.
    append_journal(
        &options.journal_path,
        &Entry {
            op: "begin".into(),
            batch: batch.clone(),
            ts_ms: now_ms(),
            id: None,
            reference: None,
            src: None,
            dst: None,
            bytes: 0,
            status: None,
            reason: None,
            items: planned.clone(),
        },
    )?;

    let mut results = Vec::with_capacity(items.len());

    for (item, plan) in items.iter().zip(planned.iter()) {
        let mut result = ExecResult {
            path: item.path.clone(),
            dst: None,
            status: ExecStatus::Skipped,
            reason: None,
            bytes: item.size,
        };

        if item.tier == Tier::Keep || item.tier == Tier::Locked {
            result.status = ExecStatus::Refused;
            result.reason = Some(format!("{} folders cannot be quarantined", item.tier.as_str()));
            results.push(result);
            continue;
        }

        if is_drive_root(&item.path) {
            result.status = ExecStatus::Refused;
            result.reason = Some("refusing to move a drive root".into());
            results.push(result);
            continue;
        }

        if is_inside(&item.path, &options.quarantine_root.to_string_lossy()) {
            result.status = ExecStatus::Refused;
            result.reason = Some("path is inside the quarantine folder".into());
            results.push(result);
            continue;
        }

        let source_root = volume_root(&item.path);
        let destination_root = volume_root(&options.quarantine_root.to_string_lossy());
        if source_root.to_lowercase() != destination_root.to_lowercase() {
            result.status = ExecStatus::Refused;
            result.reason = Some(format!(
                "quarantine is on {destination_root} but this item is on {source_root}; \
                 a cross-volume move would copy, not rename"
            ));
            results.push(result);
            continue;
        }

        let metadata = match fs::symlink_metadata(&item.path) {
            Ok(metadata) => metadata,
            Err(_) => {
                result.status = ExecStatus::Skipped;
                result.reason = Some("path no longer exists".into());
                results.push(result);
                continue;
            }
        };

        if metadata.file_type().is_symlink() {
            result.status = ExecStatus::Refused;
            result.reason = Some("symbolic links and junctions are not moved".into());
            results.push(result);
            continue;
        }

        let dst = match unique_destination(Path::new(&plan.dst)) {
            Ok(dst) => dst,
            Err(error) => {
                result.status = ExecStatus::Skipped;
                result.reason = Some(error.to_string());
                results.push(result);
                continue;
            }
        };

        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        match fs::rename(&item.path, &dst) {
            Ok(()) => {
                result.status = ExecStatus::Quarantined;
                result.dst = Some(dst.to_string_lossy().to_string());
                append_journal(
                    &options.journal_path,
                    &Entry {
                        op: "quarantine".into(),
                        batch: batch.clone(),
                        ts_ms: now_ms(),
                        id: Some(plan.id.clone()),
                        reference: None,
                        src: Some(item.path.clone()),
                        dst: Some(dst.to_string_lossy().to_string()),
                        bytes: item.size,
                        status: Some("ok".into()),
                        reason: None,
                        items: Vec::new(),
                    },
                )?;
            }
            Err(error) => {
                let reason = describe_io_error(&error);
                result.status = ExecStatus::Skipped;
                result.reason = Some(reason.clone());
                append_journal(
                    &options.journal_path,
                    &Entry {
                        op: "quarantine".into(),
                        batch: batch.clone(),
                        ts_ms: now_ms(),
                        id: Some(plan.id.clone()),
                        reference: None,
                        src: Some(item.path.clone()),
                        dst: Some(dst.to_string_lossy().to_string()),
                        bytes: item.size,
                        status: Some("failed".into()),
                        reason: Some(reason),
                        items: Vec::new(),
                    },
                )?;
            }
        }

        results.push(result);
    }

    let bytes = results
        .iter()
        .filter(|r| r.status == ExecStatus::Quarantined)
        .map(|r| r.bytes)
        .sum();

    Ok(BatchResult {
        batch,
        items: results,
        bytes,
    })
}

/// Replay the journal into the current state of every staged folder.
pub fn list_quarantine(journal_path: &Path) -> Vec<QuarantineItem> {
    let entries = read_journal(journal_path);

    let mut planned: Vec<Planned> = Vec::new();
    let mut quarantined: Vec<(Entry, bool)> = Vec::new();

    for entry in &entries {
        match entry.op.as_str() {
            "begin" => planned.extend(entry.items.iter().cloned()),
            "quarantine" => quarantined.push((
                entry.clone(),
                entry.status.as_deref() == Some("ok"),
            )),
            _ => {}
        }
    }

    let mut outcomes: std::collections::HashMap<String, QuarantineStatus> =
        std::collections::HashMap::new();
    for entry in &entries {
        if entry.op != "restore" && entry.op != "purge" {
            continue;
        }
        if entry.status.as_deref() != Some("ok") {
            continue;
        }
        if let Some(reference) = &entry.reference {
            outcomes.insert(
                reference.clone(),
                if entry.op == "restore" {
                    QuarantineStatus::Restored
                } else {
                    QuarantineStatus::Purged
                },
            );
        }
    }

    let mut items = Vec::new();
    for (entry, ok) in &quarantined {
        if !ok {
            continue;
        }
        let Some(id) = &entry.id else { continue };
        let dst = entry.dst.clone().unwrap_or_default();
        let mut status = outcomes
            .get(id)
            .copied()
            .unwrap_or(QuarantineStatus::Quarantined);
        if status == QuarantineStatus::Quarantined && !Path::new(&dst).exists() {
            status = QuarantineStatus::Missing;
        }

        items.push(QuarantineItem {
            id: id.clone(),
            src: entry.src.clone().unwrap_or_default(),
            dst,
            bytes: entry.bytes,
            batch: entry.batch.clone(),
            ts_ms: entry.ts_ms,
            status,
        });
    }

    let finished: std::collections::HashSet<String> = quarantined
        .iter()
        .filter_map(|(entry, _)| entry.id.clone())
        .chain(outcomes.keys().cloned())
        .collect();

    for plan in planned {
        if finished.contains(&plan.id) {
            continue;
        }
        items.push(QuarantineItem {
            id: plan.id,
            src: plan.src,
            dst: plan.dst,
            bytes: plan.bytes,
            batch: String::new(),
            ts_ms: 0,
            status: QuarantineStatus::Interrupted,
        });
    }

    items.sort_by_key(|item| item.ts_ms);
    items
}

/// Put staged folders back exactly where they came from.
pub fn restore(ids: &[String], journal_path: &Path) -> std::io::Result<BatchResult> {
    let items = list_quarantine(journal_path);
    let mut results = Vec::new();

    for id in ids {
        let Some(item) = items.iter().find(|i| &i.id == id) else {
            results.push(ExecResult {
                path: id.clone(),
                dst: None,
                status: ExecStatus::Skipped,
                reason: Some("unknown item".into()),
                bytes: 0,
            });
            continue;
        };

        let mut result = ExecResult {
            path: item.src.clone(),
            dst: None,
            status: ExecStatus::Skipped,
            reason: None,
            bytes: item.bytes,
        };

        if item.status != QuarantineStatus::Quarantined {
            result.reason = Some(format!("item is {}", item.status.as_str()));
            results.push(result);
            continue;
        }
        if !Path::new(&item.dst).exists() {
            result.reason = Some("staged copy is missing".into());
            results.push(result);
            continue;
        }
        if Path::new(&item.src).exists() {
            result.reason = Some("something already exists at the original path".into());
            results.push(result);
            continue;
        }

        if let Some(parent) = Path::new(&item.src).parent() {
            fs::create_dir_all(parent)?;
        }

        match fs::rename(&item.dst, &item.src) {
            Ok(()) => {
                result.status = ExecStatus::Restored;
                append_journal(
                    journal_path,
                    &Entry {
                        op: "restore".into(),
                        batch: item.batch.clone(),
                        ts_ms: now_ms(),
                        id: Some(format!("{}", now_nanos().unwrap_or(0))),
                        reference: Some(item.id.clone()),
                        src: Some(item.src.clone()),
                        dst: Some(item.dst.clone()),
                        bytes: item.bytes,
                        status: Some("ok".into()),
                        reason: None,
                        items: Vec::new(),
                    },
                )?;
            }
            Err(error) => {
                result.reason = Some(describe_io_error(&error));
            }
        }

        results.push(result);
    }

    let bytes = results
        .iter()
        .filter(|r| r.status == ExecStatus::Restored)
        .map(|r| r.bytes)
        .sum();

    Ok(BatchResult {
        batch: "restore".into(),
        items: results,
        bytes,
    })
}

/// What is actually inside a staged folder, for the confirmation screen.
#[derive(Debug, Clone)]
pub struct StagedDetail {
    pub id: String,
    pub src: String,
    pub file_count: u64,
    pub top_files: Vec<(String, u64)>,
    pub truncated: bool,
}

const MAX_FILES_INSPECTED: u64 = 20_000;
const TOP_FILES_KEPT: usize = 5;

/// Read the contents of staged folders before offering to destroy them.
///
/// "Delete 12.4 GB" is a number; "delete these 10 files, including
/// UROCHECK VIDEO DEMO.mp4" is a decision. Capped so a 300k-file folder cannot
/// stall the confirmation.
pub fn inspect_staged(items: &[QuarantineItem]) -> Vec<StagedDetail> {
    items
        .iter()
        .map(|item| {
            let mut file_count = 0u64;
            let mut top_files: Vec<(String, u64)> = Vec::new();
            let mut truncated = false;
            let mut stack = vec![PathBuf::from(&item.dst)];

            while let Some(dir) = stack.pop() {
                let Ok(entries) = fs::read_dir(&dir) else {
                    continue;
                };

                for entry in entries.flatten() {
                    if file_count >= MAX_FILES_INSPECTED {
                        truncated = true;
                        break;
                    }
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    if file_type.is_dir() {
                        stack.push(entry.path());
                        continue;
                    }
                    if !file_type.is_file() {
                        continue;
                    }
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };

                    file_count += 1;
                    top_files.push((entry.file_name().to_string_lossy().to_string(), metadata.len()));
                    top_files.sort_by(|a, b| b.1.cmp(&a.1));
                    top_files.truncate(TOP_FILES_KEPT);
                }

                if truncated {
                    break;
                }
            }

            StagedDetail {
                id: item.id.clone(),
                src: item.src.clone(),
                file_count,
                top_files,
                truncated,
            }
        })
        .collect()
}

/// Delete staged copies. This is the step that actually frees space.
pub fn purge(ids: &[String], journal_path: &Path) -> std::io::Result<BatchResult> {
    let items = list_quarantine(journal_path);
    let mut results = Vec::new();

    for id in ids {
        let Some(item) = items.iter().find(|i| &i.id == id) else {
            results.push(ExecResult {
                path: id.clone(),
                dst: None,
                status: ExecStatus::Skipped,
                reason: Some("unknown item".into()),
                bytes: 0,
            });
            continue;
        };

        let mut result = ExecResult {
            path: item.src.clone(),
            dst: None,
            status: ExecStatus::Skipped,
            reason: None,
            bytes: item.bytes,
        };

        if item.status != QuarantineStatus::Quarantined {
            result.reason = Some(format!("item is {}", item.status.as_str()));
            results.push(result);
            continue;
        }
        if !Path::new(&item.dst).exists() {
            result.reason = Some("staged copy is missing".into());
            results.push(result);
            continue;
        }

        let removed = if Path::new(&item.dst).is_dir() {
            fs::remove_dir_all(&item.dst)
        } else {
            fs::remove_file(&item.dst)
        };

        match removed {
            Ok(()) => {
                result.status = ExecStatus::Purged;
                append_journal(
                    journal_path,
                    &Entry {
                        op: "purge".into(),
                        batch: item.batch.clone(),
                        ts_ms: now_ms(),
                        id: Some(format!("{}", now_nanos().unwrap_or(0))),
                        reference: Some(item.id.clone()),
                        src: Some(item.src.clone()),
                        dst: Some(item.dst.clone()),
                        bytes: item.bytes,
                        status: Some("ok".into()),
                        reason: None,
                        items: Vec::new(),
                    },
                )?;
            }
            Err(error) => {
                result.reason = Some(describe_io_error(&error));
            }
        }

        results.push(result);
    }

    let bytes = results
        .iter()
        .filter(|r| r.status == ExecStatus::Purged)
        .map(|r| r.bytes)
        .sum();

    Ok(BatchResult {
        batch: "purge".into(),
        items: results,
        bytes,
    })
}

/// Mirror the original absolute path beneath the batch folder, drive included,
/// so a restore is a mechanical reverse mapping.
pub fn staged_path_for(quarantine_root: &Path, batch: &str, src: &str) -> PathBuf {
    let (drive, rest) = split_drive(src);
    let mut path = quarantine_root.join(batch);
    if !drive.is_empty() {
        path = path.join(drive);
    }
    for segment in rest {
        path = path.join(segment);
    }
    path
}

fn split_drive(path: &str) -> (String, Vec<String>) {
    let trimmed = path.trim_start_matches('\\');
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        let drive = trimmed[..1].to_string();
        let rest = trimmed[2..]
            .split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        (drive, rest)
    } else {
        (
            String::new(),
            path.split(['\\', '/'])
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
}

fn volume_root(path: &str) -> String {
    let trimmed = path.trim_start_matches('\\');
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        format!("{}:\\", &trimmed[..1])
    } else {
        String::new()
    }
}

fn is_drive_root(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    trimmed.len() == 2 && trimmed.as_bytes()[1] == b':'
}

fn is_inside(child: &str, parent: &str) -> bool {
    let child = normalize(child);
    let parent = normalize(parent);
    child == parent || child.starts_with(&format!("{parent}\\"))
}

fn unique_destination(desired: &Path) -> std::io::Result<PathBuf> {
    if !desired.exists() {
        return Ok(desired.to_path_buf());
    }
    for suffix in 2..1000 {
        let candidate = PathBuf::from(format!("{} ({suffix})", desired.to_string_lossy()));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("no free destination for {}", desired.display()),
    ))
}

fn describe_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => {
            "in use by another program — close it and retry".into()
        }
        std::io::ErrorKind::NotFound => "path no longer exists".into(),
        _ => error.to_string(),
    }
}

fn append_journal(path: &Path, entry: &Entry) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(entry).unwrap_or_default();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

fn read_journal(path: &Path) -> Vec<Entry> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<Entry>(&line) {
            entries.push(entry);
        }
    }
    entries
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_nanos() -> Option<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}
