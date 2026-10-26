use std::collections::{BTreeSet, HashMap, HashSet};

use regex::Regex;

use crate::tokens::{expand, TokenMap};
use crate::types::{DirEntry, Rule, Tier};

/// A rule with its matchers pre-expanded and its regex compiled once.
pub struct CompiledRule {
    pub id: String,
    pub tier: Tier,
    pub category: String,
    pub reason: String,
    pub impact: Option<String>,
    pub display: bool,
    pub min_bytes: Option<u64>,
    pub requires_sibling: Option<String>,
    prefix: Option<String>,
    name: Option<String>,
    regex: Option<Regex>,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: String,
    /// Bytes attributable to this folder excluding nested candidates.
    pub size: u64,
    /// Recursive size of the whole folder.
    pub total_size: u64,
    pub files: u64,
    pub mtime_ms: i64,
    pub tier: Tier,
    pub category: String,
    pub reason: String,
    pub rule_id: String,
    pub depth: usize,
    /// True when this folder contains data that must not be removed wholesale.
    pub blocked: bool,
    pub blocked_bytes: u64,
    pub blocked_by: Vec<String>,
}

pub fn compile(rules: &[Rule], tokens: &TokenMap) -> Vec<CompiledRule> {
    rules
        .iter()
        .map(|r| CompiledRule {
            id: r.id.clone(),
            tier: r.tier,
            category: r.category.clone(),
            reason: r.reason.clone(),
            impact: r.impact.clone(),
            display: r.display,
            min_bytes: r.min_bytes,
            requires_sibling: r.requires_sibling.clone(),
            prefix: r.matcher.prefix.as_deref().map(|p| normalize(&expand(p, tokens))),
            name: r.matcher.name.as_deref().map(|n| normalize(n)),
            regex: r.matcher.regex.as_deref().and_then(|re| Regex::new(re).ok()),
        })
        .collect()
}

/// Every matcher a rule declares must pass. A rule with no matcher never fires.
pub fn match_rule<'a>(
    path: &str,
    size: u64,
    rules: &'a [CompiledRule],
    parent_has_package_json: bool,
) -> Option<&'a CompiledRule> {
    let normalized = normalize(path);

    for rule in rules {
        if let Some(min) = rule.min_bytes {
            if size < min {
                continue;
            }
        }

        // A `node_modules` is only regenerable when a manifest sits beside it.
        if rule.requires_sibling.as_deref() == Some("package.json") && !parent_has_package_json {
            continue;
        }

        let mut has_matcher = false;
        let mut ok = true;

        if let Some(prefix) = &rule.prefix {
            has_matcher = true;
            ok = normalized == *prefix || normalized.starts_with(&format!("{prefix}\\"))
                || normalized.starts_with(&format!("{prefix}/"));
        }
        if ok {
            if let Some(name) = &rule.name {
                has_matcher = true;
                ok = base_name(&normalized) == *name;
            }
        }
        if ok {
            if let Some(regex) = &rule.regex {
                has_matcher = true;
                ok = regex.is_match(path);
            }
        }

        if has_matcher && ok {
            return Some(rule);
        }
    }

    None
}

/// Resolve review candidates.
///
/// Two problems this solves, both covered by tests:
///
/// * **Double counting.** Sizes are recursive, so `CapCut` and `CapCut\Cache`
///   would each claim the same bytes. Each candidate reports an *exclusive*
///   size: its own bytes minus nested candidate groups.
///
/// * **Protected data inside removable data.** `CapCut\Projects` (KEEP) sits
///   inside `CapCut` (REVIEW). Removing the parent would destroy the child, so
///   the parent is reported blocked, with the bytes and paths named.
pub fn find_candidates(dirs: &[DirEntry], rules: &[CompiledRule], min_exclusive: u64) -> Vec<Candidate> {
    let pkg_paths: HashSet<String> = dirs
        .iter()
        .filter(|d| d.has_package_json)
        .map(|d| normalize(&d.path))
        .collect();

    let mut relevant: Vec<&DirEntry> = dirs.iter().filter(|d| d.size >= min_exclusive).collect();
    relevant.sort_by_key(|d| depth_of(&d.path));

    // 1. Which rule matches each folder. A prefix rule matches every descendant,
    //    so only the highest folder in a run becomes a group.
    let mut matched: HashMap<String, usize> = HashMap::new();
    for d in &relevant {
        let parent_has_pkg = parent_of(&d.path)
            .map(|p| pkg_paths.contains(&normalize(&p)))
            .unwrap_or(false);

        let Some(rule) = match_rule(&d.path, d.size, rules, parent_has_pkg) else {
            continue;
        };
        let key = normalize(&d.path);

        let same_rule_above = nearest_matched(&matched, &key)
            .map(|ancestor| matched[&ancestor] == rule_index(rules, &rule.id))
            .unwrap_or(false);
        if same_rule_above {
            continue;
        }

        matched.insert(key, rule_index(rules, &rule.id));
    }

    // 2. Fold sizes upward. Deepest first, so a child is accounted for before
    //    its parent is measured.
    let mut by_depth_desc: Vec<&DirEntry> = relevant.clone();
    by_depth_desc.sort_by_key(|d| std::cmp::Reverse(depth_of(&d.path)));

    let mut size_carry: HashMap<String, u64> = HashMap::new();
    let mut prot_carry: HashMap<String, u64> = HashMap::new();
    let mut exclusive: HashMap<String, u64> = HashMap::new();
    let mut protected: HashMap<String, u64> = HashMap::new();
    let mut blocked_by: HashMap<String, BTreeSet<String>> = HashMap::new();

    for d in &by_depth_desc {
        let key = normalize(&d.path);
        let Some(&rule_idx) = matched.get(&key) else {
            continue;
        };
        let rule = &rules[rule_idx];

        let own = d.size.saturating_sub(*size_carry.get(&key).unwrap_or(&0));
        exclusive.insert(key.clone(), own);

        let is_protected = !rule.display && rule.tier.is_protective();
        let prot = if is_protected { d.size } else { 0 } + prot_carry.get(&key).copied().unwrap_or(0);
        protected.insert(key.clone(), prot);

        if is_protected {
            let mut ancestor = nearest_matched(&matched, &key);
            while let Some(current) = ancestor {
                blocked_by
                    .entry(current.clone())
                    .or_default()
                    .insert(d.path.clone());
                ancestor = nearest_matched(&matched, &current);
            }
        }

        if let Some(ancestor) = nearest_matched(&matched, &key) {
            *size_carry.entry(ancestor.clone()).or_insert(0) += d.size;
            *prot_carry.entry(ancestor).or_insert(0) += prot;
        }
    }

    // 3. Emit, in descending size so the UI shows the best wins first.
    let mut out = Vec::new();
    for d in &relevant {
        let key = normalize(&d.path);
        let Some(&rule_idx) = matched.get(&key) else {
            continue;
        };
        let rule = &rules[rule_idx];
        let own = *exclusive.get(&key).unwrap_or(&0);
        let prot = *protected.get(&key).unwrap_or(&0);

        // Display rules colour a map; they are never review items.
        if rule.display {
            continue;
        }
        if own < min_exclusive {
            continue;
        }
        if rule.tier == Tier::Locked && d.size < 1024 * 1024 * 1024 {
            continue;
        }

        let blocked = prot > 0 && matches!(rule.tier, Tier::SafePurge | Tier::Review);
        let blocked_paths: Vec<String> = blocked_by
            .get(&key)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();

        out.push(Candidate {
            path: d.path.clone(),
            size: own,
            total_size: d.size,
            files: d.files,
            mtime_ms: d.mtime_ms,
            tier: if blocked { Tier::Keep } else { rule.tier },
            category: rule.category.clone(),
            reason: if blocked {
                format!(
                    "{} — but it also contains {} protected item(s), so it cannot be removed as a whole.",
                    rule.reason,
                    blocked_paths.len()
                )
            } else {
                rule.reason.clone()
            },
            rule_id: rule.id.clone(),
            depth: depth_of(&d.path),
            blocked,
            blocked_bytes: prot,
            blocked_by: blocked_paths,
        });
    }

    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

fn rule_index(rules: &[CompiledRule], id: &str) -> usize {
    rules.iter().position(|r| r.id == id).unwrap_or(usize::MAX)
}

fn nearest_matched(matched: &HashMap<String, usize>, path: &str) -> Option<String> {
    let mut current = parent_of(path)?;
    loop {
        let key = normalize(&current);
        if matched.contains_key(&key) {
            return Some(key);
        }
        current = parent_of(&current)?;
    }
}

pub(crate) fn normalize(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).to_ascii_lowercase()
}

fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub(crate) fn depth_of(path: &str) -> usize {
    path.bytes().filter(|b| *b == b'\\').count()
}

pub(crate) fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let bytes = trimmed.as_bytes();
    if bytes.len() <= 2 && bytes.get(1) == Some(&b':') {
        return None; // drive root
    }
    let idx = trimmed.rfind('\\')?;
    if idx == 2 {
        return Some(trimmed[..3].to_string()); // `C:\`
    }
    if idx < 2 {
        return None;
    }
    Some(trimmed[..idx].to_string())
}
