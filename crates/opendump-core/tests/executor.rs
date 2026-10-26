use std::fs;
use std::path::{Path, PathBuf};

use opendump_core::{
    list_quarantine, purge, quarantine, restore, ExecItem, ExecOptions, ExecStatus,
    QuarantineStatus, Tier,
};

fn workspace(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("opendump-exec-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create workspace");
    path
}

fn options(root: &Path) -> ExecOptions {
    ExecOptions {
        quarantine_root: root.join("quarantine"),
        journal_path: root.join("journal.jsonl"),
        batch_id: Some("b1".into()),
    }
}

fn make_folder(root: &Path, relative: &str, file: &str, body: &str) -> String {
    let path = root.join(relative);
    fs::create_dir_all(&path).expect("create folder");
    fs::write(path.join(file), body).expect("write file");
    path.to_string_lossy().to_string()
}

fn item(path: &str, tier: Tier) -> ExecItem {
    ExecItem {
        path: path.to_string(),
        tier,
        rule_id: "test-rule".into(),
        size: 1024,
    }
}

#[test]
fn quarantine_moves_the_folder_and_keeps_its_contents() {
    let root = workspace("move");
    let target = make_folder(&root, "data/cache", "a.bin", "hello");
    let opts = options(&root);

    let result = quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");

    assert_eq!(result.items[0].status, ExecStatus::Quarantined);
    assert!(!Path::new(&target).exists(), "source is gone");
    let dst = result.items[0].dst.clone().expect("destination");
    assert_eq!(fs::read_to_string(Path::new(&dst).join("a.bin")).unwrap(), "hello");

    let staged = list_quarantine(&opts.journal_path);
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].src, target);
    assert_eq!(staged[0].status, QuarantineStatus::Quarantined);

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn protected_tiers_are_refused() {
    let root = workspace("refuse-tier");
    let target = make_folder(&root, "data/precious", "a.bin", "keep me");
    let opts = options(&root);

    let keep = quarantine(&[item(&target, Tier::Keep)], &opts).expect("quarantine");
    let locked = quarantine(&[item(&target, Tier::Locked)], &opts).expect("quarantine");

    assert_eq!(keep.items[0].status, ExecStatus::Refused);
    assert_eq!(locked.items[0].status, ExecStatus::Refused);
    assert!(Path::new(&target).exists(), "nothing moved");

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn a_drive_root_is_refused() {
    let root = workspace("refuse-root");
    let opts = options(&root);

    let result = quarantine(&[item("C:\\", Tier::SafePurge)], &opts).expect("quarantine");
    assert_eq!(result.items[0].status, ExecStatus::Refused);

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn a_path_inside_the_quarantine_is_refused() {
    let root = workspace("refuse-inside");
    let opts = options(&root);
    let inside = make_folder(&root, "quarantine/b1/loop", "a.bin", "x");

    let result = quarantine(&[item(&inside, Tier::SafePurge)], &opts).expect("quarantine");

    assert_eq!(result.items[0].status, ExecStatus::Refused);
    assert!(Path::new(&inside).exists());

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn a_cross_volume_move_is_refused() {
    let root = workspace("refuse-volume");
    let target = make_folder(&root, "data/cache", "a.bin", "x");

    let mut opts = options(&root);
    opts.quarantine_root = PathBuf::from("Z:\\elsewhere");
    opts.batch_id = Some("b1".into());

    let result = quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");

    assert_eq!(result.items[0].status, ExecStatus::Refused);
    assert!(result.items[0]
        .reason
        .as_deref()
        .unwrap_or_default()
        .contains("cross-volume"));
    assert!(Path::new(&target).exists());

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn a_missing_path_is_skipped_not_failed() {
    let root = workspace("missing");
    let opts = options(&root);
    let missing = root.join("data/missing").to_string_lossy().to_string();

    let result = quarantine(&[item(&missing, Tier::SafePurge)], &opts).expect("quarantine");

    assert_eq!(result.items[0].status, ExecStatus::Skipped);
    assert!(result.items[0].reason.is_some());

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn an_existing_staged_folder_is_not_overwritten() {
    let root = workspace("collision");
    let target = make_folder(&root, "data/cache", "a.bin", "new");
    let opts = options(&root);

    let occupied = opendump_core::executor::staged_path_for(&opts.quarantine_root, "b1", &target);
    fs::create_dir_all(&occupied).expect("occupy");
    fs::write(occupied.join("a.bin"), "older").expect("write");

    let result = quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");

    assert_eq!(result.items[0].status, ExecStatus::Quarantined);
    assert_ne!(result.items[0].dst.as_deref(), Some(occupied.to_string_lossy().as_ref()));
    assert_eq!(fs::read_to_string(occupied.join("a.bin")).unwrap(), "older");

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn restore_puts_the_folder_back_where_it_came_from() {
    let root = workspace("restore");
    let target = make_folder(&root, "data/cache", "a.bin", "hello");
    let opts = options(&root);

    quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");
    let staged = list_quarantine(&opts.journal_path);
    let restored = restore(&[staged[0].id.clone()], &opts.journal_path).expect("restore");

    assert_eq!(restored.items[0].status, ExecStatus::Restored);
    assert_eq!(fs::read_to_string(Path::new(&target).join("a.bin")).unwrap(), "hello");
    assert_eq!(
        list_quarantine(&opts.journal_path)[0].status,
        QuarantineStatus::Restored
    );

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn restore_refuses_when_something_reappeared_at_the_original_path() {
    let root = workspace("restore-clash");
    let target = make_folder(&root, "data/cache", "a.bin", "original");
    let opts = options(&root);

    quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");
    make_folder(&root, "data/cache", "a.bin", "recreated");

    let staged = list_quarantine(&opts.journal_path);
    let result = restore(&[staged[0].id.clone()], &opts.journal_path).expect("restore");

    assert_eq!(result.items[0].status, ExecStatus::Skipped);
    assert_eq!(fs::read_to_string(Path::new(&target).join("a.bin")).unwrap(), "recreated");

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn purge_deletes_the_staged_copy_and_records_it() {
    let root = workspace("purge");
    let target = make_folder(&root, "data/cache", "a.bin", "bye");
    let opts = options(&root);

    quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");
    let staged = list_quarantine(&opts.journal_path);
    let dst = staged[0].dst.clone();

    let result = purge(&[staged[0].id.clone()], &opts.journal_path).expect("purge");

    assert_eq!(result.items[0].status, ExecStatus::Purged);
    assert!(!Path::new(&dst).exists(), "staged copy removed");
    assert_eq!(list_quarantine(&opts.journal_path)[0].status, QuarantineStatus::Purged);

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn a_staged_copy_removed_by_hand_reports_missing() {
    let root = workspace("missing-staged");
    let target = make_folder(&root, "data/cache", "a.bin", "x");
    let opts = options(&root);

    quarantine(&[item(&target, Tier::SafePurge)], &opts).expect("quarantine");
    let staged = list_quarantine(&opts.journal_path);
    fs::remove_dir_all(&staged[0].dst).expect("remove by hand");

    let after = list_quarantine(&opts.journal_path);
    assert_eq!(after[0].status, QuarantineStatus::Missing);

    let result = purge(&[after[0].id.clone()], &opts.journal_path).expect("purge");
    assert_eq!(result.items[0].status, ExecStatus::Skipped);

    fs::remove_dir_all(&root).expect("cleanup");
}
