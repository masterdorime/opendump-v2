//! Filesystem walk.
//!
//! This is the deliberately boring version: `read_dir` plus `metadata`, producing
//! one [`DirEntry`] per directory with **recursive** sizes.
//!
//! The interesting version — parsing the NTFS MFT directly, which is the whole
//! reason this project exists in Rust — replaces this module without touching
//! `rules.rs`. That is the boundary worth preserving.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::rules::normalize;
use crate::types::DirEntry;

/// Directories that are loops or already surfaced elsewhere by Windows.
const SKIP_NAMES: [&str; 3] = ["$recycle.bin", "system volume information", "config.msi"];

/// Legacy junctions inside a user profile (`Application Data` → `AppData\Roaming`).
/// Node and Rust both report these as symlinks, which the walk already skips;
/// this is belt-and-braces so a missed flag cannot double-count a whole profile.
const LEGACY_JUNCTIONS: [&str; 13] = [
    "application data",
    "documents and settings",
    "local settings",
    "my documents",
    "nethood",
    "printhood",
    "recent",
    "sendto",
    "start menu",
    "templates",
    "cookies",
    "history",
    "temporary internet files",
];

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub root: String,
    pub dirs: Vec<DirEntry>,
    pub total_bytes: u64,
    pub total_files: u64,
    pub skipped: u64,
    pub duration_ms: u128,
}

/// What the walk knows part-way through. `bytes` is the sum of file sizes seen
/// so far, which is a real measure of work done — not an estimate.
#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub folders: u64,
    pub files: u64,
    pub bytes: u64,
    pub current: String,
}

struct Record {
    path: String,
    bytes: u64,
    files: u64,
    has_package_json: bool,
}

/// Walk `root` and return every directory with its recursive size.
pub fn scan(root: &Path) -> ScanReport {
    scan_with_progress(root, |_| {})
}

/// Same walk, reporting progress as it goes.
///
/// The callback fires about every 256 folders — frequent enough for a smooth
/// bar, rare enough that the channel is not the bottleneck.
pub fn scan_with_progress<F>(root: &Path, mut on_progress: F) -> ScanReport
where
    F: FnMut(ScanProgress),
{
    let started = Instant::now();
    let root_string = root.to_string_lossy().to_string();

    let mut records: Vec<Record> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut skipped: u64 = 0;
    let mut seen_folders: u64 = 0;
    let mut seen_files: u64 = 0;
    let mut seen_bytes: u64 = 0;

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let mut bytes = 0u64;
        let mut files = 0u64;
        let mut has_package_json = false;

        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };

            if file_type.is_symlink() {
                skipped += 1;
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();

            if file_type.is_dir() {
                if SKIP_NAMES.contains(&lower.as_str()) {
                    skipped += 1;
                    continue;
                }
                if LEGACY_JUNCTIONS.contains(&lower.as_str()) && is_profile_root(&dir) {
                    skipped += 1;
                    continue;
                }
                stack.push(entry.path());
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            if lower == "package.json" {
                has_package_json = true;
            }

            if let Ok(metadata) = entry.metadata() {
                bytes += metadata.len();
                files += 1;
                seen_bytes += metadata.len();
                seen_files += 1;
            }
        }

        let path_string = dir.to_string_lossy().to_string();
        index.insert(normalize(&path_string), records.len());
        records.push(Record {
            path: path_string,
            bytes,
            files,
            has_package_json,
        });

        seen_folders += 1;
        if seen_folders % 256 == 0 {
            on_progress(ScanProgress {
                folders: seen_folders,
                files: seen_files,
                bytes: seen_bytes,
                current: dir.to_string_lossy().to_string(),
            });
        }
    }

    // Fold direct sizes upward: a child path is always longer than its parent, so
    // processing by descending length folds every child before its parent.
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(records[i].path.len()));

    for &i in &order {
        let parent = match crate::rules::parent_of(&records[i].path) {
            Some(parent) => parent,
            None => continue,
        };
        let Some(&parent_index) = index.get(&normalize(&parent)) else {
            continue;
        };
        let extra = (records[i].bytes, records[i].files);
        records[parent_index].bytes += extra.0;
        records[parent_index].files += extra.1;
    }

    let dirs: Vec<DirEntry> = records
        .iter()
        .map(|r| DirEntry {
            path: r.path.clone(),
            size: r.bytes,
            files: r.files,
            has_package_json: r.has_package_json,
        })
        .collect();

    let total_bytes = dirs
        .iter()
        .find(|d| normalize(&d.path) == normalize(&root_string))
        .map(|d| d.size)
        .unwrap_or(0);
    let total_files = dirs
        .iter()
        .find(|d| normalize(&d.path) == normalize(&root_string))
        .map(|d| d.files)
        .unwrap_or(0);

    ScanReport {
        root: root_string,
        dirs,
        total_bytes,
        total_files,
        skipped,
        duration_ms: started.elapsed().as_millis(),
    }
}

fn is_profile_root(dir: &Path) -> bool {
    let parts: Vec<String> = dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    parts.len() == 3 && parts[1] == "users"
}

