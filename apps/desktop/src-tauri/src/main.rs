//! opendump desktop shell.
//!
//! All classification and filesystem work happens in `opendump-core`. This file
//! is deliberately thin: it exposes commands, embeds the rule set, and refuses
//! to quarantine anything the rules do not classify as removable.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use opendump_core::{
    compile, default_tokens, find_candidates, list_quarantine, match_rule, purge, quarantine,
    restore, scan, ExecItem, ExecOptions, Rule, Tier,
};

/// The rule set is embedded at compile time, so the binary has no runtime file
/// dependency and cannot run with a mismatched rules file.
const RULES_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../rules/rules.json"));

#[derive(Deserialize)]
struct RulesDocument {
    rules: Vec<Rule>,
}

#[derive(Serialize)]
struct CandidateView {
    path: String,
    size: u64,
    tier: String,
    reason: String,
    category: String,
    rule_id: String,
    blocked: bool,
    blocked_bytes: u64,
}

#[derive(Serialize)]
struct ScanView {
    root: String,
    folders: u64,
    files: u64,
    bytes: u64,
    duration_ms: u64,
    candidates: Vec<CandidateView>,
}

#[derive(Serialize)]
struct ExecView {
    path: String,
    status: String,
    reason: Option<String>,
    bytes: u64,
}

#[derive(Serialize)]
struct BatchView {
    batch: String,
    bytes: u64,
    items: Vec<ExecView>,
}

#[derive(Serialize)]
struct StagedView {
    id: String,
    src: String,
    bytes: u64,
    status: String,
}

fn base_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("opendump-v2"))
        .unwrap_or_else(|_| PathBuf::from(".opendump-v2"))
}

fn exec_options() -> ExecOptions {
    ExecOptions {
        quarantine_root: base_dir().join("quarantine"),
        journal_path: base_dir().join("journal.jsonl"),
        batch_id: None,
    }
}

fn compiled_rules() -> Result<Vec<opendump_core::CompiledRule>, String> {
    let document: RulesDocument =
        serde_json::from_str(RULES_JSON).map_err(|error| format!("bundled rules are invalid: {error}"))?;
    Ok(compile(&document.rules, &default_tokens()))
}

fn default_drive() -> String {
    format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()))
}

/// Classify one path the same way the scanner world does, returning the item and
/// the rule that accepted it.
fn classify(path: &str, compiled: &[opendump_core::CompiledRule]) -> Result<(ExecItem, String), String> {
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

    let rule = match_rule(path, size, compiled, parent_has_pkg)
        .ok_or_else(|| "no rule classifies this folder".to_string())?;

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

#[tauri::command]
fn scan_drive(path: Option<String>) -> Result<ScanView, String> {
    let root = path.unwrap_or_else(default_drive);
    let compiled = compiled_rules()?;

    let report = scan(Path::new(&root));
    let candidates = find_candidates(&report.dirs, &compiled, 1024 * 1024)
        .into_iter()
        .map(|candidate| CandidateView {
            path: candidate.path,
            size: candidate.size,
            tier: candidate.tier.as_str().to_string(),
            reason: candidate.reason,
            category: candidate.category,
            rule_id: candidate.rule_id,
            blocked: candidate.blocked,
            blocked_bytes: candidate.blocked_bytes,
        })
        .collect();

    Ok(ScanView {
        root: report.root,
        folders: report.dirs.len() as u64,
        files: report.total_files,
        bytes: report.total_bytes,
        duration_ms: report.duration_ms as u64,
        candidates,
    })
}

#[tauri::command]
fn quarantine_paths(paths: Vec<String>) -> Result<BatchView, String> {
    let compiled = compiled_rules()?;
    let mut items = Vec::new();
    let mut skip_notes = Vec::new();

    for path in &paths {
        match classify(path, &compiled) {
            Ok((item, reason)) => items.push((item, reason)),
            Err(reason) => skip_notes.push(ExecView {
                path: path.clone(),
                status: "refused".into(),
                reason: Some(reason),
                bytes: 0,
            }),
        }
    }

    let exec_items: Vec<ExecItem> = items.into_iter().map(|(item, _)| item).collect();
    let result = quarantine(&exec_items, &exec_options()).map_err(|error| error.to_string())?;

    let mut views: Vec<ExecView> = result
        .items
        .iter()
        .map(|item| ExecView {
            path: item.path.clone(),
            status: item.status.as_str().to_string(),
            reason: item.reason.clone(),
            bytes: item.bytes,
        })
        .collect();
    views.extend(skip_notes);

    Ok(BatchView {
        batch: result.batch,
        bytes: result.bytes,
        items: views,
    })
}

#[tauri::command]
fn staged_items() -> Result<Vec<StagedView>, String> {
    let journal = base_dir().join("journal.jsonl");
    Ok(list_quarantine(&journal)
        .into_iter()
        .map(|item| StagedView {
            id: item.id,
            src: item.src,
            bytes: item.bytes,
            status: item.status.as_str().to_string(),
        })
        .collect())
}

#[tauri::command]
fn restore_all() -> Result<BatchView, String> {
    let journal = base_dir().join("journal.jsonl");
    let ids: Vec<String> = list_quarantine(&journal)
        .into_iter()
        .filter(|item| item.status.as_str() == "quarantined")
        .map(|item| item.id)
        .collect();

    let result = restore(&ids, &journal).map_err(|error| error.to_string())?;
    Ok(BatchView {
        batch: result.batch,
        bytes: result.bytes,
        items: result
            .items
            .iter()
            .map(|item| ExecView {
                path: item.path.clone(),
                status: item.status.as_str().to_string(),
                reason: item.reason.clone(),
                bytes: item.bytes,
            })
            .collect(),
    })
}

#[tauri::command]
fn purge_all() -> Result<BatchView, String> {
    let journal = base_dir().join("journal.jsonl");
    let ids: Vec<String> = list_quarantine(&journal)
        .into_iter()
        .filter(|item| item.status.as_str() == "quarantined")
        .map(|item| item.id)
        .collect();

    let result = purge(&ids, &journal).map_err(|error| error.to_string())?;
    Ok(BatchView {
        batch: result.batch,
        bytes: result.bytes,
        items: result
            .items
            .iter()
            .map(|item| ExecView {
                path: item.path.clone(),
                status: item.status.as_str().to_string(),
                reason: item.reason.clone(),
                bytes: item.bytes,
            })
            .collect(),
    })
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            scan_drive,
            quarantine_paths,
            staged_items,
            restore_all,
            purge_all
        ])
        .run(tauri::generate_context!())
        .expect("error while running opendump");
}
