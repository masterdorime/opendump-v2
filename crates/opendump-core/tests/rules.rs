use opendump_core::{
    compile, default_tokens, depth_of, find_candidates, match_rule, parent_of, DirEntry, Rule,
    RuleMatch, Tier,
};

const MB: u64 = 1024 * 1024;

fn rule(id: &str, tier: Tier, matcher: RuleMatch) -> Rule {
    Rule {
        id: id.to_string(),
        tier,
        category: "test".into(),
        reason: format!("reason for {id}"),
        impact: None,
        display: false,
        min_bytes: None,
        requires_sibling: None,
        matcher,
    }
}

fn prefix(value: &str) -> RuleMatch {
    RuleMatch {
        prefix: Some(value.to_string()),
        ..Default::default()
    }
}

fn name(value: &str) -> RuleMatch {
    RuleMatch {
        name: Some(value.to_string()),
        ..Default::default()
    }
}

fn dir(path: &str, size: u64) -> DirEntry {
    DirEntry::new(path, size)
}

// ---------------------------------------------------------------- AND matching

#[test]
fn matchers_on_a_rule_are_anded() {
    let rules = vec![
        rule(
            "keep-installed-modules",
            Tier::Keep,
            RuleMatch {
                prefix: Some("C:\\Program Files".into()),
                name: Some("node_modules".into()),
                regex: None,
            },
        ),
        rule("purge-modules", Tier::SafePurge, name("node_modules")),
    ];
    let compiled = compile(&rules, &default_tokens());

    let inside = match_rule("C:\\Program Files\\Adobe\\js\\node_modules", 100 * MB, &compiled, true);
    assert_eq!(inside.map(|i| compiled[i].id.as_str()), Some("keep-installed-modules"));

    let project = match_rule("C:\\work\\proj\\node_modules", 100 * MB, &compiled, true);
    assert_eq!(project.map(|i| compiled[i].id.as_str()), Some("purge-modules"));

    // The prefix alone must not match: a name matcher is also declared.
    let other = match_rule("C:\\Program Files\\something-else", 100 * MB, &compiled, true);
    assert!(other.is_none());
}

#[test]
fn a_rule_with_no_matchers_never_fires() {
    let rules = vec![rule("empty", Tier::Review, RuleMatch::default())];
    let compiled = compile(&rules, &default_tokens());
    assert!(match_rule("C:\\anything", 10 * MB, &compiled, false).is_none());
}

#[test]
fn requires_sibling_asks_about_the_parent() {
    let mut modules = rule("purge-modules", Tier::SafePurge, name("node_modules"));
    modules.requires_sibling = Some("package.json".into());
    let compiled = compile(&[modules], &default_tokens());

    assert!(match_rule("C:\\work\\proj\\node_modules", 100 * MB, &compiled, false).is_none());
    assert!(match_rule("C:\\work\\proj\\node_modules", 100 * MB, &compiled, true).is_some());
}

#[test]
fn tokens_expand_before_matching() {
    let mut tokens = default_tokens();
    tokens.insert("LOCALAPPDATA".into(), "D:\\Profiles\\alex\\AppData\\Local".into());
    let compiled = compile(&[rule("cache", Tier::SafePurge, prefix("%LOCALAPPDATA%\\npm-cache"))], &tokens);

    assert!(match_rule("D:\\Profiles\\alex\\AppData\\Local\\npm-cache", MB, &compiled, false).is_some());
    assert!(match_rule("C:\\Users\\alex\\AppData\\Local\\npm-cache", MB, &compiled, false).is_none());
}

// ------------------------------------------------------------ candidate sizing

#[test]
fn a_parent_only_reports_bytes_its_children_do_not_claim() {
    let dirs = vec![
        dir("C:\\", 1000 * MB),
        dir("C:\\A", 600 * MB),
        dir("C:\\A\\cache", 400 * MB),
        dir("C:\\A\\keep", 150 * MB),
        dir("C:\\B", 300 * MB),
    ];

    let rules = vec![
        rule("cache", Tier::SafePurge, prefix("C:\\A\\cache")),
        rule("keep", Tier::Keep, prefix("C:\\A\\keep")),
        rule("a", Tier::Review, prefix("C:\\A")),
        rule("b", Tier::SafePurge, prefix("C:\\B")),
    ];

    let candidates = find_candidates(&dirs, &compile(&rules, &default_tokens()), 1);
    let find = |path: &str| candidates.iter().find(|c| c.path.eq_ignore_ascii_case(path)).cloned();

    let a = find("C:\\A").expect("A is a candidate");
    assert_eq!(a.size, 50 * MB, "A only owns what its children do not");

    assert_eq!(find("C:\\A\\cache").unwrap().size, 400 * MB);
    assert_eq!(find("C:\\A\\keep").unwrap().size, 150 * MB);
    assert_eq!(find("C:\\B").unwrap().size, 300 * MB);
}

#[test]
fn protected_children_block_their_parent() {
    let dirs = vec![
        dir("C:\\A", 600 * MB),
        dir("C:\\A\\cache", 400 * MB),
        dir("C:\\A\\keep", 150 * MB),
    ];
    let rules = vec![
        rule("cache", Tier::SafePurge, prefix("C:\\A\\cache")),
        rule("keep", Tier::Keep, prefix("C:\\A\\keep")),
        rule("a", Tier::Review, prefix("C:\\A")),
    ];

    let candidates = find_candidates(&dirs, &compile(&rules, &default_tokens()), 1);
    let a = candidates.iter().find(|c| c.path == "C:\\A").expect("A listed");

    assert!(a.blocked, "a folder holding KEEP data must not be removable whole");
    assert_eq!(a.tier, Tier::Keep);
    assert_eq!(a.blocked_bytes, 150 * MB);
    assert_eq!(a.blocked_by, vec!["C:\\A\\keep".to_string()]);
}

#[test]
fn the_sum_of_candidates_never_exceeds_the_tree() {
    let dirs = vec![
        dir("C:\\", 1000 * MB),
        dir("C:\\A", 600 * MB),
        dir("C:\\A\\cache", 400 * MB),
        dir("C:\\A\\keep", 150 * MB),
        dir("C:\\B", 300 * MB),
    ];
    let rules = vec![
        rule("cache", Tier::SafePurge, prefix("C:\\A\\cache")),
        rule("keep", Tier::Keep, prefix("C:\\A\\keep")),
        rule("a", Tier::Review, prefix("C:\\A")),
        rule("b", Tier::SafePurge, prefix("C:\\B")),
    ];

    let candidates = find_candidates(&dirs, &compile(&rules, &default_tokens()), 1);
    let total: u64 = candidates.iter().map(|c| c.size).sum();
    assert!(total <= 1000 * MB);
    assert_eq!(total, 900 * MB, "A(50) + cache(400) + keep(150) + B(300)");
}

#[test]
fn a_prefix_rule_collapses_to_its_highest_folder() {
    let dirs = vec![
        dir("C:\\cache", 500 * MB),
        dir("C:\\cache\\http-v2\\8", 300 * MB),
        dir("C:\\cache\\http-v2\\8\\0", 200 * MB),
    ];
    let rules = vec![rule("cache", Tier::SafePurge, prefix("C:\\cache"))];

    let candidates = find_candidates(&dirs, &compile(&rules, &default_tokens()), 1);
    assert_eq!(candidates.len(), 1, "one run of the same rule is one candidate");
    assert_eq!(candidates[0].path, "C:\\cache");
    assert_eq!(candidates[0].size, 500 * MB);
}

#[test]
fn display_rules_colour_but_never_appear_as_candidates() {
    let mut display_rule = rule("profile", Tier::Keep, prefix("C:\\Users\\alex"));
    display_rule.display = true;
    let compiled = compile(&[display_rule], &default_tokens());

    let candidates = find_candidates(&[dir("C:\\Users\\alex", 900 * MB)], &compiled, 1);
    assert!(candidates.is_empty());
}

// ------------------------------------------------------------- serialisation

#[test]
fn the_shipped_rule_shape_deserialises() {
    let json = r#"{
      "rules": [
        {
          "id": "purge-node-modules",
          "tier": "SAFE_PURGE",
          "category": "Build artifact",
          "reason": "Installed JS dependencies.",
          "requiresSibling": "package.json",
          "display": false,
          "minBytes": 10485760,
          "match": { "name": "node_modules" }
        },
        {
          "id": "lock-winsxs",
          "tier": "LOCKED",
          "category": "Windows component store",
          "reason": "Servicing data.",
          "match": { "prefix": "%WINDIR%\\WinSxS" }
        }
      ]
    }"#;

    #[derive(serde::Deserialize)]
    struct Document {
        rules: Vec<Rule>,
    }

    let parsed: Document = serde_json::from_str(json).expect("shipped rules must parse");
    assert_eq!(parsed.rules.len(), 2);
    assert_eq!(parsed.rules[0].tier, Tier::SafePurge);
    assert_eq!(parsed.rules[0].min_bytes, Some(10_485_760));
    assert_eq!(parsed.rules[0].requires_sibling.as_deref(), Some("package.json"));
    assert_eq!(parsed.rules[1].tier, Tier::Locked);
    assert_eq!(parsed.rules[1].matcher.prefix.as_deref(), Some("%WINDIR%\\WinSxS"));
}

#[test]
fn parent_of_handles_drive_roots_and_both_separators() {
    // The treemap groups directories by parent, so these edges are load-bearing.
    assert_eq!(parent_of("C:\\"), None);
    assert_eq!(parent_of("C:"), None);
    assert_eq!(parent_of("C:\\Users"), Some("C:\\".to_string()));
    assert_eq!(parent_of("C:\\Users\\you"), Some("C:\\Users".to_string()));

    // Users paste forward slashes; the separator is normalised rather than missed.
    assert_eq!(parent_of("C:/Users/you"), Some("C:\\Users".to_string()));
    assert_eq!(parent_of("C:\\Users\\you\\"), Some("C:\\Users".to_string()));
}

#[test]
fn depth_of_is_a_separator_count_not_a_level_number() {
    // It exists to order ancestors before descendants, so equal counts at the
    // drive root (`C:\`) and its direct child (`C:\Users`) are fine. Assert the
    // real behaviour rather than an idealised one: the treemap does not use it.
    assert_eq!(depth_of("C:\\"), 1);
    assert_eq!(depth_of("C:\\Users"), 1);
    assert_eq!(depth_of("C:/Users/you"), 2);
    assert_eq!(depth_of("C:\\Users\\you"), 2);

    // The ordering property that actually matters.
    assert!(depth_of("C:\\Users\\you") > depth_of("C:\\Users"));
}
