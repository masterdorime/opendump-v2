//! `opendump` — command-line front end for the Rust core.
//!
//! Deliberately read-only. There is no executor in this crate and no code path
//! that removes or renames anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use opendump_core::{
    compile, default_tokens, find_candidates, scan, Candidate, Rule, Tier,
};

#[derive(serde::Deserialize)]
struct RulesDocument {
    rules: Vec<Rule>,
}

struct Options {
    path: PathBuf,
    rules: PathBuf,
    top: usize,
    min_bytes: u64,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        usage();
        return ExitCode::SUCCESS;
    }

    if args[0] != "scan" {
        eprintln!("unknown command: {}\n", args[0]);
        usage();
        return ExitCode::from(2);
    }

    let options = match parse_options(&args[1..]) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n");
            usage();
            return ExitCode::from(2);
        }
    };

    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "opendump {} — Rust core\n\
         \n\
         USAGE:\n    \
         opendump scan <path> [--rules <rules.json>] [--top <n>] [--min-mb <n>]\n\
         \n\
         EXAMPLES:\n    \
         opendump scan \"C:\\\\Users\\\\you\\\\AppData\\\\Local\"\n    \
         opendump scan \"C:\\\\\" --top 40 --min-mb 100\n\
         \n\
         Read-only: this command scans and classifies. It never removes anything.\n",
        env!("CARGO_PKG_VERSION")
    );
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut path: Option<PathBuf> = None;
    let mut rules = PathBuf::from("rules/rules.json");
    let mut top = 20usize;
    let mut min_bytes = 1024 * 1024;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--rules" => {
                i += 1;
                rules = PathBuf::from(args.get(i).ok_or("--rules needs a path")?);
            }
            "--top" => {
                i += 1;
                top = args
                    .get(i)
                    .ok_or("--top needs a number")?
                    .parse()
                    .map_err(|_| "--top needs a number")?;
            }
            "--min-mb" => {
                i += 1;
                let mb: u64 = args
                    .get(i)
                    .ok_or("--min-mb needs a number")?
                    .parse()
                    .map_err(|_| "--min-mb needs a number")?;
                min_bytes = mb * 1024 * 1024;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                if path.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                path = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }

    Ok(Options {
        path: path.ok_or("a path to scan is required")?,
        rules,
        top,
        min_bytes,
    })
}

fn run(options: &Options) -> Result<(), String> {
    if !options.path.exists() {
        return Err(format!("no such path: {}", options.path.display()));
    }

    let rules_path = resolve_rules(&options.rules);
    let raw = std::fs::read_to_string(&rules_path)
        .map_err(|e| format!("cannot read {}: {e}", rules_path.display()))?;
    let document: RulesDocument =
        serde_json::from_str(&raw).map_err(|e| format!("{} is not valid rules JSON: {e}", rules_path.display()))?;

    let compiled = compile(&document.rules, &default_tokens());

    println!("opendump {} — rust core", env!("CARGO_PKG_VERSION"));
    println!("root      {}", options.path.display());
    println!("rules     {} ({} loaded)", rules_path.display(), document.rules.len());
    println!();

    let report = scan(&options.path);
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

    let candidates = find_candidates(&report.dirs, &compiled, options.min_bytes);
    print_summary(&candidates);
    print_top(&candidates, options.top);

    Ok(())
}

fn print_summary(candidates: &[Candidate]) {
    let mut by_tier: HashMap<&'static str, (usize, u64)> = HashMap::new();
    for candidate in candidates {
        let entry = by_tier.entry(candidate.tier.as_str()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += candidate.size;
    }

    println!("\ncandidates (>= {} exclusive)", bytes(1024 * 1024));
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

fn resolve_rules(requested: &Path) -> PathBuf {
    if requested.exists() {
        return requested.to_path_buf();
    }
    // Fall back to a rules file beside the executable, which is how the file
    // will ship once this gains a packaged build.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("rules/rules.json");
            if beside.exists() {
                return beside;
            }
        }
    }
    requested.to_path_buf()
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
