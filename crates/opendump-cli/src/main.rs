//! `opendump` — command-line front end for the Rust core.
//!
//! `scan` is read-only. `quarantine`, `restore` and `purge` write, and every one
//! of them requires an explicit confirmation flag. `quarantine` additionally
//! refuses any folder the rule set does not classify as removable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use opendump_core::{
    compile, default_tokens, find_candidates, list_quarantine, match_rule, purge, quarantine,
    restore, scan, Candidate, ExecItem, ExecOptions, Rule, Tier,
};

#[derive(serde::Deserialize)]
struct RulesDocument {
    rules: Vec<Rule>,
}

/// Flags that take a value; everything else starting with `--` is boolean.
const VALUE_FLAGS: [&str; 5] = ["--rules", "--top", "--min-mb", "--journal", "--quarantine"];

struct Args {
    positionals: Vec<String>,
    flags: HashMap<String, Option<String>>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut positionals = Vec::new();
        let mut flags: HashMap<String, Option<String>> = HashMap::new();

        let mut i = 0;
        while i < raw.len() {
            let arg = &raw[i];
            if arg.starts_with("--") {
                if VALUE_FLAGS.contains(&arg.as_str()) {
                    i += 1;
                    let value = raw.get(i).ok_or_else(|| format!("{arg} needs a value"))?;
                    flags.insert(arg.clone(), Some(value.clone()));
                } else {
                    flags.insert(arg.clone(), None);
                }
            } else {
                positionals.push(arg.clone());
            }
            i += 1;
        }

        Ok(Args {
            positionals,
            flags,
        })
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.contains_key(flag)
    }

    fn value(&self, flag: &str) -> Option<&str> {
        self.flags.get(flag).and_then(|v| v.as_deref())
    }

    fn number(&self, flag: &str, fallback: u64) -> Result<u64, String> {
        match self.value(flag) {
            None => Ok(fallback),
            Some(raw) => raw
                .parse()
                .map_err(|_| format!("{flag} needs a number, got {raw}")),
        }
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();

    match raw.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            usage();
            ExitCode::SUCCESS
        }
        Some("scan") => scan_command(&raw[1..]),
        Some("quarantine") => quarantine_command(&raw[1..]),
        Some("list") => list_command(&raw[1..]),
        Some("restore") => restore_command(&raw[1..]),
        Some("purge") => purge_command(&raw[1..]),
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "opendump {} — Rust core\n\
         \n\
         USAGE:\n  \
         opendump scan <path> [--rules <file>] [--top <n>] [--min-mb <n>]\n  \
         opendump quarantine <path>... [--allow-review] [--yes]\n  \
         opendump list\n  \
         opendump restore <id>... | --all\n  \
         opendump purge <id>... | --all --yes\n\
         \n\
         STATE:\n  \
         journal and quarantine live in %APPDATA%\\opendump-v2\n  \
         override with --journal <file> and --quarantine <dir>\n\
         \n\
         `scan` never writes. `quarantine` refuses any folder the rules do not\n\
         classify as removable, and does nothing without --yes.\n",
        env!("CARGO_PKG_VERSION")
    );
}

// ------------------------------------------------------------------ commands

fn scan_command(raw: &[String]) -> ExitCode {
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };

    let path = match args.positionals.first() {
        Some(path) => PathBuf::from(path),
        None => return fail("scan needs a path"),
    };
    let top = match args.number("--top", 20) {
        Ok(value) => value as usize,
        Err(message) => return fail(&message),
    };
    let min_bytes = match args.number("--min-mb", 1) {
        Ok(value) => value * 1024 * 1024,
        Err(message) => return fail(&message),
    };

    let document = match load_rules(args.value("--rules")) {
        Ok(document) => document,
        Err(message) => return fail(&message),
    };
    let compiled = compile(&document.rules, &default_tokens());

    if !path.exists() {
        return fail(&format!("no such path: {}", path.display()));
    }

    println!("opendump {} — rust core", env!("CARGO_PKG_VERSION"));
    println!("root      {}", path.display());
    println!("rules     {} loaded", document.rules.len());
    println!();

    let report = scan(&path);
    let seconds = report.duration_ms as f64 / 1000.0;
    let per_second = if seconds > 0.0 {
        (report.dirs.len() as f64 / seconds).round() as u64
    } else {
        0
    };

    println!("folders   {}", thousands(report.dirs.len() as u64));
    println!("files     {}", thousands(report.total_files));
    println!("size      {}", bytes(report.total_bytes));
    println!("scan      {seconds:.2}s  ({} folders/s)", thousands(per_second));

    let candidates = find_candidates(&report.dirs, &compiled, min_bytes);
    print_summary(&candidates);
    print_top(&candidates, top);
    ExitCode::SUCCESS
}

fn quarantine_command(raw: &[String]) -> ExitCode {
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };
    if args.positionals.is_empty() {
        return fail("quarantine needs at least one path");
    }

    let document = match load_rules(args.value("--rules")) {
        Ok(document) => document,
        Err(message) => return fail(&message),
    };
    let compiled = compile(&document.rules, &default_tokens());
    let allow_review = args.has("--allow-review");

    let mut items = Vec::new();
    let mut refusals = Vec::new();

    for raw_path in &args.positionals {
        let path = PathBuf::from(raw_path);
        if !path.exists() {
            refusals.push(format!("{raw_path}: does not exist"));
            continue;
        }

        let report = scan(&path);
        let size = report
            .dirs
            .iter()
            .find(|d| Path::new(&d.path) == path)
            .map(|d| d.size)
            .unwrap_or(0);

        let parent_has_pkg = path
            .parent()
            .map(|parent| parent.join("package.json").is_file())
            .unwrap_or(false);

        let rule = match_rule(raw_path, size, &compiled, parent_has_pkg);
        let Some(rule) = rule else {
            refusals.push(format!("{raw_path}: no rule classifies this folder"));
            continue;
        };

        let accepted = rule.tier == Tier::SafePurge
            || (allow_review && rule.tier == Tier::Review);

        if !accepted {
            let hint = if rule.tier == Tier::Review && !allow_review {
                " — pass --allow-review to accept REVIEW folders"
            } else {
                ""
            };
            refusals.push(format!(
                "{raw_path}: classified {} by {}{hint}",
                rule.tier.as_str(),
                rule.id
            ));
            continue;
        }

        items.push((
            ExecItem {
                path: raw_path.clone(),
                tier: rule.tier,
                rule_id: rule.id.clone(),
                size,
            },
            rule.reason.clone(),
        ));
    }

    for refusal in &refusals {
        eprintln!("  refused  {refusal}");
    }

    if items.is_empty() {
        eprintln!("\nnothing to quarantine");
        return ExitCode::from(1);
    }

    let total: u64 = items.iter().map(|(item, _)| item.size).sum();
    println!("plan — {} folder(s), {}", items.len(), bytes(total));
    for (item, reason) in &items {
        println!(
            "  {:<11} {:>10}  {}",
            item.tier.as_str(),
            bytes(item.size),
            item.path
        );
        println!("              └─ {reason}");
    }

    if !args.has("--yes") {
        println!("\nthis would move them into quarantine. re-run with --yes to do it.");
        return ExitCode::from(1);
    }

    let options = exec_options(&args);
    let exec_items: Vec<ExecItem> = items.into_iter().map(|(item, _)| item).collect();

    match quarantine(&exec_items, &options) {
        Ok(result) => {
            println!("\nbatch {}", result.batch);
            for item in &result.items {
                match item.status.as_str() {
                    "quarantined" => println!("  moved    {}", item.path),
                    _ => println!("  {}  {}  ({})", item.status.as_str(), item.path, item.reason.clone().unwrap_or_default()),
                }
            }
            println!(
                "\n{} moved, {} staged. nothing has been deleted.",
                bytes(result.bytes),
                result.items.iter().filter(|i| i.status.as_str() == "quarantined").count()
            );
            println!("`opendump list` to review, `opendump purge --all --yes` to reclaim.");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn list_command(raw: &[String]) -> ExitCode {
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };

    let items = list_quarantine(&journal_path(&args));
    if items.is_empty() {
        println!("quarantine is empty");
        return ExitCode::SUCCESS;
    }

    let mut staged = 0u64;
    println!("{:<38} {:<12} {:>10}  {}", "id", "status", "size", "source");
    for item in &items {
        if item.status.as_str() == "quarantined" {
            staged += item.bytes;
        }
        println!(
            "{:<38} {:<12} {:>10}  {}",
            item.id,
            item.status.as_str(),
            bytes_opt(item.bytes),
            item.src
        );
    }
    println!("\n{} held ({})", bytes(staged), items.len());
    ExitCode::SUCCESS
}

fn restore_command(raw: &[String]) -> ExitCode {
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };

    let journal = journal_path(&args);
    let ids = match select_ids(&args, &journal) {
        Ok(ids) => ids,
        Err(message) => return fail(&message),
    };

    match restore(&ids, &journal) {
        Ok(result) => {
            for item in &result.items {
                if item.status.as_str() == "restored" {
                    println!("  restored  {}", item.path);
                } else {
                    println!("  skipped   {}  ({})", item.path, item.reason.clone().unwrap_or_default());
                }
            }
            println!("\n{} restored", bytes(result.bytes));
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn purge_command(raw: &[String]) -> ExitCode {
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(message) => return fail(&message),
    };

    let journal = journal_path(&args);
    let ids = match select_ids(&args, &journal) {
        Ok(ids) => ids,
        Err(message) => return fail(&message),
    };

    let items = list_quarantine(&journal);
    let total: u64 = items
        .iter()
        .filter(|item| ids.contains(&item.id) && item.status.as_str() == "quarantined")
        .map(|item| item.bytes)
        .sum();

    if !args.has("--yes") {
        println!("this would permanently delete {} folder(s), {}.", ids.len(), bytes(total));
        println!("there is no undo after this. re-run with --yes to confirm.");
        return ExitCode::from(1);
    }

    match purge(&ids, &journal) {
        Ok(result) => {
            for item in &result.items {
                if item.status.as_str() == "purged" {
                    println!("  deleted   {}", item.path);
                } else {
                    println!("  skipped   {}  ({})", item.path, item.reason.clone().unwrap_or_default());
                }
            }
            println!("\n{} reclaimed", bytes(result.bytes));
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

// ------------------------------------------------------------------- helpers

fn select_ids(args: &Args, journal: &Path) -> Result<Vec<String>, String> {
    if args.has("--all") {
        let ids: Vec<String> = list_quarantine(journal)
            .into_iter()
            .filter(|item| item.status.as_str() == "quarantined")
            .map(|item| item.id)
            .collect();
        if ids.is_empty() {
            return Err("nothing is staged".into());
        }
        return Ok(ids);
    }
    if args.positionals.is_empty() {
        return Err("pass one or more ids, or --all".into());
    }
    Ok(args.positionals.clone())
}

fn exec_options(args: &Args) -> ExecOptions {
    ExecOptions {
        quarantine_root: args
            .value("--quarantine")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir().join("quarantine")),
        journal_path: journal_path(args),
        batch_id: None,
    }
}

fn journal_path(args: &Args) -> PathBuf {
    args.value("--journal")
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir().join("journal.jsonl"))
}

fn base_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("opendump-v2"))
        .unwrap_or_else(|_| PathBuf::from(".opendump-v2"))
}

fn load_rules(requested: Option<&str>) -> Result<RulesDocument, String> {
    let path = match requested {
        Some(path) => PathBuf::from(path),
        None => resolve_rules(),
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("{} is not valid rules JSON: {error}", path.display()))
}

fn resolve_rules() -> PathBuf {
    let local = PathBuf::from("rules/rules.json");
    if local.exists() {
        return local;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("rules/rules.json");
            if beside.exists() {
                return beside;
            }
        }
    }
    local
}

fn print_summary(candidates: &[Candidate]) {
    let mut by_tier: HashMap<&'static str, (usize, u64)> = HashMap::new();
    for candidate in candidates {
        let entry = by_tier.entry(candidate.tier.as_str()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += candidate.size;
    }

    println!("\ncandidates");
    for tier in [Tier::SafePurge, Tier::Review, Tier::Keep, Tier::Locked] {
        let (count, total) = by_tier.get(tier.as_str()).copied().unwrap_or((0, 0));
        println!(
            "  {:<11} {:>4}   {:>10}",
            tier.as_str(),
            thousands(count as u64),
            bytes(total)
        );
    }
}

fn print_top(candidates: &[Candidate], top: usize) {
    if candidates.is_empty() {
        println!("\nno candidates above the reporting floor");
        return;
    }

    println!("\ntop {} by reclaimed bytes", top.min(candidates.len()));
    for candidate in candidates.iter().take(top) {
        let blocked = if candidate.blocked {
            format!("  [holds {} protected]", bytes(candidate.blocked_bytes))
        } else {
            String::new()
        };
        println!(
            "  {:<11} {:>10}  {}{}",
            candidate.tier.as_str(),
            bytes(candidate.size),
            candidate.path,
            blocked
        );
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(1)
}

fn bytes_opt(value: u64) -> String {
    bytes(value)
}

fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
