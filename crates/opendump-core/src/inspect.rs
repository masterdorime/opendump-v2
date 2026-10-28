//! Folder inspection: what is actually inside a folder, as opposed to what its
//! name suggests.
//!
//! This exists because a rule can only see a path. `CapCut\User Data\Cache` was
//! classified SAFE_PURGE until someone noticed it held a human-named
//! `UROCHECK VIDEO DEMO … .mp4`. Names lie; contents do not.
//!
//! Bounded on purpose: one folder, at most `cap` files, no recursion depth
//! limit beyond the filesystem. It runs on demand from the UI, never during a
//! scan.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Extensions a person would recognise as their own work.
const HUMAN_EXTENSIONS: [&str; 20] = [
    "mp4", "mov", "mkv", "avi", "webm", "jpg", "jpeg", "png", "heic", "gif", "psd", "blend", "aep",
    "prproj", "doc", "docx", "xls", "xlsx", "pdf", "txt",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDetail {
    pub files_seen: u64,
    pub bytes_seen: u64,
    /// extension, file count, bytes. Most common first.
    pub extensions: Vec<(String, u64, u64)>,
    /// Largest files, biggest first.
    pub top_files: Vec<(String, u64)>,
    pub hashed: u64,
    pub human: u64,
    /// Filled when the contents disagree with the folder's name.
    pub concern: Option<String>,
    /// True when the walk stopped at the cap.
    pub truncated: bool,
}

/// Inspect `path`, reading at most `cap` files.
pub fn inspect_folder(path: &Path, cap: u64) -> std::io::Result<FolderDetail> {
    let mut stack = vec![path.to_path_buf()];
    let mut extensions: HashMap<String, (u64, u64)> = HashMap::new();
    let mut top_files: Vec<(String, u64)> = Vec::new();

    let mut files_seen = 0u64;
    let mut bytes_seen = 0u64;
    let mut hashed = 0u64;
    let mut human = 0u64;
    let mut human_media = 0u64;

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            if files_seen >= cap {
                return Ok(finish(
                    files_seen,
                    bytes_seen,
                    extensions,
                    top_files,
                    hashed,
                    human,
                    human_media,
                    true,
                ));
            }

            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
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
            let size = metadata.len();
            files_seen += 1;
            bytes_seen += size;

            let name = entry.file_name().to_string_lossy().to_string();
            let (stem, extension) = match name.rsplit_once('.') {
                Some((stem, extension)) if !stem.is_empty() => {
                    (stem.to_string(), extension.to_ascii_lowercase())
                }
                _ => (name.clone(), String::new()),
            };

            let slot = extensions.entry(extension.clone()).or_insert((0, 0));
            slot.0 += 1;
            slot.1 += size;

            if looks_hashed(&stem) {
                hashed += 1;
            } else {
                human += 1;
                if HUMAN_EXTENSIONS.contains(&extension.as_str()) {
                    human_media += 1;
                }
            }

            top_files.push((name, size));
            top_files.sort_by(|left, right| right.1.cmp(&left.1));
            top_files.truncate(5);
        }
    }

    Ok(finish(
        files_seen,
        bytes_seen,
        extensions,
        top_files,
        hashed,
        human,
        human_media,
        false,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    files_seen: u64,
    bytes_seen: u64,
    extensions: HashMap<String, (u64, u64)>,
    top_files: Vec<(String, u64)>,
    hashed: u64,
    human: u64,
    human_media: u64,
    truncated: bool,
) -> FolderDetail {
    let mut extensions: Vec<(String, u64, u64)> = extensions
        .into_iter()
        .map(|(name, (count, bytes))| (name, count, bytes))
        .collect();
    extensions.sort_by(|left, right| right.1.cmp(&left.1));
    extensions.truncate(8);

    let total = hashed + human;
    let concern = if total == 0 {
        None
    } else if hashed * 10 >= total * 9 {
        Some(format!(
            "{} of {} files have hash-like names — this looks like a cache.",
            hashed, total
        ))
    } else if human_media >= 3 && human_media * 5 >= total {
        Some(format!(
            "{} human-named media or document files here — check before removing.",
            human_media
        ))
    } else {
        None
    };

    FolderDetail {
        files_seen,
        bytes_seen,
        extensions,
        top_files,
        hashed,
        human,
        concern,
        truncated,
    }
}

/// A name is "hashed" when it looks machine-generated: a long hex run, a UUID,
/// a pure digit run, or long base64-ish noise.
pub fn looks_hashed(stem: &str) -> bool {
    if stem.len() < 8 {
        return false;
    }

    let hex = stem
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .count();

    if stem.chars().all(|character| character.is_ascii_digit()) {
        return stem.len() >= 6;
    }

    // 32-hex UUID with dashes.
    if stem.len() >= 32 && hex * 10 >= stem.len() * 7 && stem.contains('-') {
        return true;
    }

    // Mostly hex and long: content-addressed names.
    if stem.len() >= 16 && hex * 10 >= stem.len() * 9 {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("opendump-inspect-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create");
        path
    }

    fn write(dir: &Path, name: &str, bytes: usize) {
        let mut file = fs::File::create(dir.join(name)).expect("create file");
        file.write_all(&vec![b'x'; bytes]).expect("write");
    }

    #[test]
    fn a_cache_of_hashed_names_is_called_a_cache() {
        let root = scratch("cache");
        for index in 0..12 {
            write(
                &root,
                &format!("{:016x}.bin", 0xdead_beef_0000_0000u64 + index),
                16,
            );
        }

        let detail = inspect_folder(&root, 1_000).expect("inspect");
        assert_eq!(detail.files_seen, 12);
        assert_eq!(detail.extensions[0].0, "bin");
        assert!(detail.concern.as_deref().unwrap_or("").contains("cache"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn human_named_media_is_flagged_even_when_the_folder_says_cache() {
        // The CapCut lesson: the folder name promised a cache.
        let root = scratch("media");
        write(&root, "UROCHECK VIDEO DEMO 2025.mp4", 400);
        write(&root, "final cut.mp4", 300);
        write(&root, "thumb 00912.png", 120);
        write(&root, "notes.txt", 40);

        let detail = inspect_folder(&root, 1_000).expect("inspect");
        assert_eq!(detail.human, 4);
        assert!(detail.concern.as_deref().unwrap_or("").contains("human-named"));
        assert_eq!(detail.top_files[0].0, "UROCHECK VIDEO DEMO 2025.mp4");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_cap_stops_the_walk_and_says_so() {
        let root = scratch("cap");
        for index in 0..30 {
            write(&root, &format!("file{index}.dat"), 10);
        }

        let detail = inspect_folder(&root, 10).expect("inspect");
        assert_eq!(detail.files_seen, 10);
        assert!(detail.truncated);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hashed_names_are_recognised_and_titles_are_not() {
        assert!(looks_hashed("9f86d081884c7d659a2feaa0c55ad015"));
        assert!(looks_hashed("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"));
        assert!(looks_hashed("1699999999"));
        assert!(!looks_hashed("UROCHECK VIDEO DEMO 2025"));
        assert!(!looks_hashed("Minecraft"));
        assert!(!looks_hashed("short"));
    }
}
