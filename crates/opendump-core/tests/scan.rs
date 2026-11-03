use std::fs;
use std::path::{Path, PathBuf};

use opendump_core::scan::{scan, ScanReport};

fn temp_root(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("opendump-scan-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create fixture root");
    path
}

fn size_of(report: &ScanReport, path: &Path) -> Option<u64> {
    report
        .dirs
        .iter()
        .find(|d| Path::new(&d.path) == path)
        .map(|d| d.size)
}

#[test]
fn recursive_sizes_are_summed_upward() {
    let root = temp_root("sizes");
    fs::create_dir_all(root.join("a/b")).expect("create tree");
    fs::write(root.join("top.txt"), vec![b'x'; 100]).expect("write");
    fs::write(root.join("a/one.txt"), vec![b'x'; 200]).expect("write");
    fs::write(root.join("a/b/two.txt"), vec![b'x'; 300]).expect("write");

    let report = scan(&root);

    assert_eq!(report.total_bytes, 600, "root total");
    assert_eq!(size_of(&report, &root.join("a")), Some(500), "a includes b");
    assert_eq!(size_of(&report, &root.join("a/b")), Some(300), "leaf stays leaf");
    assert_eq!(report.total_files, 3);

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn package_json_belongs_to_the_folder_that_holds_it() {
    let root = temp_root("pkg");
    fs::create_dir_all(root.join("proj/node_modules/dep")).expect("create tree");
    fs::write(root.join("proj/package.json"), "{}").expect("write manifest");

    let report = scan(&root);
    let project = report
        .dirs
        .iter()
        .find(|d| d.path.ends_with("proj"))
        .expect("project dir");
    let modules = report
        .dirs
        .iter()
        .find(|d| d.path.ends_with("node_modules"))
        .expect("node_modules dir");

    assert!(project.has_package_json, "manifest sits beside node_modules");
    assert!(
        !modules.has_package_json,
        "requiresSibling asks about the parent, not the folder itself"
    );

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn an_empty_folder_scans_to_zero() {
    let root = temp_root("empty");
    let report = scan(&root);

    assert_eq!(report.total_bytes, 0);
    assert_eq!(report.total_files, 0);
    assert_eq!(report.dirs.len(), 1, "the root is still reported");

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn an_excluded_folder_is_never_read() {
    let root = temp_root("exclude");
    fs::create_dir_all(root.join("keep")).expect("create tree");
    fs::create_dir_all(root.join("skip/deep")).expect("create tree");
    fs::write(root.join("keep/one.txt"), vec![b'x'; 100]).expect("write");
    fs::write(root.join("skip/deep/two.txt"), vec![b'x'; 900]).expect("write");

    let excluded = vec![root.join("skip").to_string_lossy().to_string()];
    let report = opendump_core::scan::scan_with_exclusions(&root, &excluded, |_| {});

    assert_eq!(report.total_bytes, 100, "only the kept file counts");
    assert_eq!(report.total_files, 1);
    assert!(
        !report.dirs.iter().any(|dir| dir.path.contains("skip")),
        "the excluded folder and its children are absent"
    );
    assert!(report.dirs.iter().any(|dir| dir.path.ends_with("keep")));

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn exclusion_matching_is_a_path_prefix_not_a_substring() {
    let excluded = vec![r"C:\Users\tristan\VMs".to_string()];

    assert!(opendump_core::scan::is_excluded(r"C:\Users\tristan\VMs", &excluded));
    assert!(opendump_core::scan::is_excluded(
        r"C:\Users\tristan\vms\win11\disk.vmdk",
        &excluded
    ));
    assert!(
        !opendump_core::scan::is_excluded(r"C:\Users\tristan\VMs-old", &excluded),
        "a sibling whose name starts with the same letters must survive"
    );
    assert!(!opendump_core::scan::is_excluded(r"C:\Users\tristan", &excluded));
}
