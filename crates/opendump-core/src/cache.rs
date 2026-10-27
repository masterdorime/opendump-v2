//! Scan cache: what lets the window open to a populated map instead of making
//! the user wait for a full walk every launch.
//!
//! Deliberately **not** v1's format. v1 wrote 58 MB of JSON and parsed the whole
//! object graph at startup; this writes a compact binary of the *derived* data
//! (the pruned map nodes and the candidates) with front-coded paths, because
//! sibling paths share almost all of their bytes.
//!
//! The file is invalidated by a hash of `rules.json`: a cache built by different
//! rules would show verdicts this build can no longer justify, so it is thrown
//! away rather than migrated.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::rules::Candidate;
use crate::types::Tier;

const MAGIC: &[u8; 8] = b"ODCACHE\x01";
const VERSION: u32 = 1;
/// A corrupt or hostile length field must not become an allocation.
const MAX_ENTRIES: u32 = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedNode {
    pub path: String,
    pub size: u64,
    pub files: u64,
    pub tier: Option<Tier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedScan {
    pub root: String,
    /// Unix seconds.
    pub scanned_at: u64,
    pub duration_ms: u64,
    pub total_bytes: u64,
    pub total_files: u64,
    pub folders: u64,
    pub nodes: Vec<CachedNode>,
    pub candidates: Vec<Candidate>,
}

/// Stable hash of the rules text, used to invalidate a cache built by other rules.
pub fn rules_hash(rules_json: &str) -> u64 {
    // FNV-1a: not cryptographic, only ever compared for equality.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in rules_json.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn tier_byte(tier: Option<Tier>) -> u8 {
    match tier {
        None => 0,
        Some(Tier::SafePurge) => 1,
        Some(Tier::Review) => 2,
        Some(Tier::Keep) => 3,
        Some(Tier::Locked) => 4,
    }
}

fn tier_from_byte(value: u8) -> Option<Tier> {
    match value {
        1 => Some(Tier::SafePurge),
        2 => Some(Tier::Review),
        3 => Some(Tier::Keep),
        4 => Some(Tier::Locked),
        _ => None,
    }
}

pub fn save(path: &Path, scan: &CachedScan, rules: u64) -> io::Result<()> {
    // First run on a clean profile: the directory does not exist yet.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut out = Vec::with_capacity(64 * 1024);

    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&rules.to_le_bytes());
    out.extend_from_slice(&scan.scanned_at.to_le_bytes());
    out.extend_from_slice(&scan.duration_ms.to_le_bytes());
    out.extend_from_slice(&scan.total_bytes.to_le_bytes());
    out.extend_from_slice(&scan.total_files.to_le_bytes());
    out.extend_from_slice(&scan.folders.to_le_bytes());
    put_str(&mut out, &scan.root)?;

    // Sorted, so neighbours share long prefixes and front-coding pays off.
    let mut nodes: Vec<&CachedNode> = scan.nodes.iter().collect();
    nodes.sort_by(|left, right| left.path.cmp(&right.path));

    put_u32(&mut out, nodes.len() as u32)?;
    let mut previous: &str = "";
    for node in &nodes {
        let shared = common_prefix(previous, &node.path);
        let suffix = &node.path[shared..];
        if suffix.len() > u16::MAX as usize || shared > u16::MAX as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "path too long"));
        }
        out.extend_from_slice(&(shared as u16).to_le_bytes());
        out.extend_from_slice(&(suffix.len() as u16).to_le_bytes());
        out.extend_from_slice(suffix.as_bytes());
        out.extend_from_slice(&node.size.to_le_bytes());
        out.extend_from_slice(&node.files.to_le_bytes());
        out.push(tier_byte(node.tier));
        previous = &node.path;
    }

    put_u32(&mut out, scan.candidates.len() as u32)?;
    for candidate in &scan.candidates {
        put_str(&mut out, &candidate.path)?;
        out.push(tier_byte(Some(candidate.tier)));
        out.extend_from_slice(&candidate.size.to_le_bytes());
        out.push(u8::from(candidate.blocked));
        out.extend_from_slice(&candidate.blocked_bytes.to_le_bytes());
        put_str(&mut out, &candidate.reason)?;
    }

    // Write to a temp file and rename: a half-written cache would be read as
    // corrupt on the next launch and force a rescan at the worst moment.
    let temporary = path.with_extension("bin.tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&out)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// Load a cache written by the same rules. `None` means "rescan", never a partial.
pub fn load(path: &Path, rules: u64) -> Option<CachedScan> {
    let bytes = fs::read(path).ok()?;
    let mut reader = Reader::new(&bytes);

    if reader.take(8)? != MAGIC {
        return None;
    }
    if reader.u32()? != VERSION || reader.u64()? != rules {
        return None;
    }

    let scanned_at = reader.u64()?;
    let duration_ms = reader.u64()?;
    let total_bytes = reader.u64()?;
    let total_files = reader.u64()?;
    let folders = reader.u64()?;
    let root = reader.string()?;

    let node_count = reader.u32()?;
    if node_count > MAX_ENTRIES {
        return None;
    }

    let mut nodes = Vec::with_capacity(node_count as usize);
    let mut previous = String::new();
    for _ in 0..node_count {
        let shared = reader.u16()? as usize;
        let suffix_len = reader.u16()? as usize;
        if shared > previous.len() {
            return None;
        }
        let suffix = reader.take(suffix_len)?;
        let suffix = std::str::from_utf8(suffix).ok()?;

        let mut path = String::with_capacity(shared + suffix_len);
        path.push_str(&previous[..shared]);
        path.push_str(suffix);

        let size = reader.u64()?;
        let files = reader.u64()?;
        let tier = tier_from_byte(reader.u8()?);

        nodes.push(CachedNode {
            path: path.clone(),
            size,
            files,
            tier,
        });
        previous = path;
    }

    let candidate_count = reader.u32()?;
    if candidate_count > MAX_ENTRIES {
        return None;
    }

    let mut candidates = Vec::with_capacity(candidate_count as usize);
    for _ in 0..candidate_count {
        let path = reader.string()?;
        let tier = tier_from_byte(reader.u8()?).unwrap_or(Tier::Review);
        let size = reader.u64()?;
        let blocked = reader.u8()? != 0;
        let blocked_bytes = reader.u64()?;
        let reason = reader.string()?;

        candidates.push(Candidate {
            path,
            size,
            tier,
            reason,
            blocked,
            blocked_bytes,
            // Not persisted: it is diagnostics for the rule engine, and the UI
            // never shows it. Reloading with it empty changes nothing visible.
            blocked_by: Vec::new(),
        });
    }

    Some(CachedScan {
        root,
        scanned_at,
        duration_ms,
        total_bytes,
        total_files,
        folders,
        nodes,
        candidates,
    })
}

fn common_prefix(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .take_while(|(a, b)| a == b)
        .count()
}

fn put_u32(out: &mut Vec<u8>, value: u32) -> io::Result<()> {
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_str(out: &mut Vec<u8>, text: &str) -> io::Result<()> {
    if text.len() > u32::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "string too long"));
    }
    put_u32(out, text.len() as u32)?;
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        let bytes = self.take(8)?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Some(u64::from_le_bytes(array))
    }

    fn string(&mut self) -> Option<String> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        Some(std::str::from_utf8(bytes).ok()?.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CachedScan {
        CachedScan {
            root: "C:\\".into(),
            scanned_at: 1_700_000_000,
            duration_ms: 91_700,
            total_bytes: 418_600_000_000,
            total_files: 1_725_000,
            folders: 367_360,
            nodes: vec![
                CachedNode {
                    path: "C:\\Users".into(),
                    size: 226_000_000_000,
                    files: 900_000,
                    tier: Some(Tier::Keep),
                },
                CachedNode {
                    path: "C:\\Users\\you\\AppData\\Local\\npm-cache".into(),
                    size: 2_200_000_000,
                    files: 30_000,
                    tier: Some(Tier::SafePurge),
                },
                CachedNode {
                    path: "C:\\Windows".into(),
                    size: 56_800_000_000,
                    files: 400_000,
                    tier: None,
                },
            ],
            candidates: vec![Candidate {
                path: "C:\\Users\\you\\AppData\\Local\\npm-cache".into(),
                size: 2_200_000_000,
                tier: Tier::SafePurge,
                reason: "npm's package cache".into(),
                blocked: false,
                blocked_bytes: 0,
                blocked_by: Vec::new(),
            }],
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("opendump-cache-{name}.bin"));
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn round_trips_every_field() {
        let path = scratch("roundtrip");
        let original = sample();
        let rules = rules_hash("rules");

        save(&path, &original, rules).expect("save");
        let loaded = load(&path, rules).expect("load");

        assert_eq!(loaded, original);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn creates_the_directory_it_is_asked_to_write_into() {
        // A clean profile has no opendump-native directory at all.
        let root = std::env::temp_dir().join("opendump-cache-nested");
        let _ = fs::remove_dir_all(&root);
        let path = root.join("deeper").join("scan-cache.bin");

        save(&path, &sample(), 7).expect("save should create the directory");
        assert!(load(&path, 7).is_some());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_different_rules_hash_is_treated_as_missing() {
        let path = scratch("rules");
        save(&path, &sample(), rules_hash("one")).expect("save");

        assert!(load(&path, rules_hash("two")).is_none());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn truncated_and_foreign_files_are_rejected_not_panicked() {
        let path = scratch("truncated");
        save(&path, &sample(), 7).expect("save");

        let bytes = fs::read(&path).expect("read");
        fs::write(&path, &bytes[..bytes.len() / 2]).expect("truncate");
        assert!(load(&path, 7).is_none());

        fs::write(&path, b"not a cache at all").expect("garbage");
        assert!(load(&path, 7).is_none());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn front_coding_beats_storing_whole_paths() {
        let path = scratch("size");
        let mut scan = sample();
        scan.nodes = (0..1_000)
            .map(|index| CachedNode {
                path: format!("C:\\Users\\you\\AppData\\Local\\SomeVeryLongFolderName\\{index}\\cache\\nested"),
                size: index as u64,
                files: 1,
                tier: None,
            })
            .collect();

        save(&path, &scan, 7).expect("save");
        let size = fs::metadata(&path).expect("metadata").len() as usize;

        let raw: usize = scan.nodes.iter().map(|node| node.path.len()).sum();
        // These synthetic paths diverge immediately after a shared prefix, so
        // this is the pessimistic case; real AppData trees share far more.
        // The claim under test is "front-coding pays", not a specific ratio.
        assert!(
            size < raw * 3 / 5,
            "cache {size} bytes vs {raw} of raw paths ({:.0}% saved); front-coding did not help enough",
            (1.0 - size as f64 / raw as f64) * 100.0
        );

        let loaded = load(&path, 7).expect("load");
        assert_eq!(loaded.nodes.len(), 1_000);
        fs::remove_file(&path).ok();
    }
}
