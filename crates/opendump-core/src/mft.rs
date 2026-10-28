//! NTFS Master File Table reader.
//!
//! The walk in `scan.rs` asks the filesystem about every entry — about two
//! million round trips on this machine. The MFT already contains that
//! information as a flat array; reading it sequentially is one pass.
//!
//! **Requires elevation.** Opening `\\.\C:` needs a raw volume handle, which
//! needs Administrator. Everything below that is pure byte parsing, so the
//! risky logic is unit-tested against synthetic buffers and only the volume
//! read itself depends on privilege.
//!
//! Scope: names, sizes, directory structure and the reparse flag. Attribute
//! lists (records fragmented across several MFT entries) are skipped and
//! reported, rather than silently guessed.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::Instant;

use crate::scan::{ScanReport, SKIP_NAMES};
use crate::types::DirEntry;

const MFT_RECORD_MAGIC: u32 = 0x454c_4946; // "FILE"
const ATTR_END: u32 = 0xFFFF_FFFF;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ROOT_RECORD: u64 = 5;
const FIRST_USER_RECORD: u64 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub bytes_per_sector: u64,
    pub bytes_per_cluster: u64,
    pub mft_lcn: u64,
    pub record_size: u64,
}

/// Where records go missing. Printed by the `--mft` probe so a wrong total can
/// be diagnosed instead of guessed at.
#[derive(Debug, Default, Clone)]
pub struct ReadStats {
    pub records: u64,
    pub in_use: u64,
    pub parsed: u64,
    pub not_in_use: u64,
    pub bad_magic: u64,
    pub bad_fixup: u64,
    pub attribute_list: u64,
    pub no_name: u64,
    pub files: u64,
    pub dirs: u64,
    pub bytes_in_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub record: u64,
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub reparse: bool,
}

/// Parse the NTFS boot sector.
pub fn parse_boot(sector: &[u8]) -> Option<VolumeInfo> {
    if sector.len() < 512 || &sector[3..11] != b"NTFS    " {
        return None;
    }

    let bytes_per_sector = u16::from_le_bytes([sector[0x0B], sector[0x0C]]) as u64;
    let sectors_per_cluster = sector[0x0D] as u64;
    let mft_lcn = u64::from_le_bytes(sector[0x30..0x38].try_into().ok()?);
    let clusters_per_record = sector[0x40] as i8;

    if bytes_per_sector < 256 || sectors_per_cluster == 0 {
        return None;
    }

    // A negative cluster count means "2^-n bytes", the usual 1024-byte record.
    let record_size = if clusters_per_record > 0 {
        clusters_per_record as u64 * bytes_per_sector * sectors_per_cluster
    } else {
        1u64 << (-clusters_per_record as u32)
    };

    if record_size < 512 || record_size > 65536 {
        return None;
    }

    Some(VolumeInfo {
        bytes_per_sector,
        bytes_per_cluster: bytes_per_sector * sectors_per_cluster,
        mft_lcn,
        record_size,
    })
}

/// Undo the update sequence array, which NTFS uses to detect torn writes.
///
/// Without this the last two bytes of every sector are the sequence number,
/// not data — which corrupts an attribute that crosses a sector boundary.
pub fn apply_fixup(record: &mut [u8], bytes_per_sector: u64) -> bool {
    let usa_offset = u16::from_le_bytes([record[4], record[5]]) as usize;
    let usa_count = u16::from_le_bytes([record[6], record[7]]) as usize;
    if usa_count == 0 || usa_offset + usa_count * 2 > record.len() {
        return false;
    }

    let sequence = [record[usa_offset], record[usa_offset + 1]];
    let sector = bytes_per_sector as usize;

    for index in 1..usa_count {
        let position = index * sector - 2;
        if position + 2 > record.len() {
            return false;
        }
        if record[position] != sequence[0] || record[position + 1] != sequence[1] {
            return false;
        }
        record[position] = record[usa_offset + index * 2];
        record[position + 1] = record[usa_offset + index * 2 + 1];
    }

    true
}

/// One $FILE_NAME attribute: a path for a file or directory. A file with hard
/// links has several, and the walk would have counted each of those paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAttr {
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub reparse: bool,
    pub size: u64,
}

/// What one MFT record contains, before it becomes path records.
#[derive(Debug, Clone, Default)]
pub struct ParsedRecord {
    /// Non-zero when this record is an extension of another (attribute list).
    pub base: u64,
    pub names: Vec<NameAttr>,
    pub data_size: Option<u64>,
}

/// Read one MFT record: every usable name, and the unnamed data stream's size.
pub fn parse_record(
    record: &[u8],
    bytes_per_sector: u64,
    stats: &mut ReadStats,
) -> Option<ParsedRecord> {
    stats.records += 1;

    if record.len() < 48 {
        return None;
    }
    if u32::from_le_bytes(record[0..4].try_into().ok()?) != MFT_RECORD_MAGIC {
        stats.bad_magic += 1;
        return None;
    }

    let mut buffer = record.to_vec();
    if !apply_fixup(&mut buffer, bytes_per_sector) {
        stats.bad_fixup += 1;
        return None;
    }

    let flags = u16::from_le_bytes([buffer[0x16], buffer[0x17]]);
    if flags & 0x01 == 0 {
        stats.not_in_use += 1;
        return None;
    }
    stats.in_use += 1;

    let base = u64::from_le_bytes(buffer[0x20..0x28].try_into().unwrap_or([0; 8]))
        & 0x0000_FFFF_FFFF_FFFF;

    let first_attribute = u16::from_le_bytes([buffer[0x14], buffer[0x15]]) as usize;
    let mut offset = first_attribute;
    let mut parsed = ParsedRecord {
        base,
        names: Vec::new(),
        data_size: None,
    };

    while offset + 16 <= buffer.len() {
        let kind = u32::from_le_bytes(buffer[offset..offset + 4].try_into().ok()?);
        if kind == ATTR_END {
            break;
        }
        let length = u32::from_le_bytes(buffer[offset + 4..offset + 8].try_into().ok()?) as usize;
        if length < 16 || offset + length > buffer.len() {
            break;
        }

        // An attribute list means some attributes live in extension records. The
        // base record still holds a name, and dropping it orphans whole subtrees.
        if kind == ATTR_ATTRIBUTE_LIST {
            stats.attribute_list += 1;
        }

        if kind == ATTR_FILE_NAME {
            let non_resident = buffer[offset + 8] != 0;
            if !non_resident {
                let value_length =
                    u32::from_le_bytes(buffer[offset + 0x10..offset + 0x14].try_into().ok()?)
                        as usize;
                let value_offset =
                    u16::from_le_bytes([buffer[offset + 0x14], buffer[offset + 0x15]]) as usize;
                let value = offset + value_offset;

                if value + 0x42 <= buffer.len() && value + value_length <= buffer.len() {
                    if let Some(name) = parse_file_name(&buffer[value..value + value_length]) {
                        parsed.names.push(name);
                    }
                }
            }
        }

        if kind == ATTR_DATA {
            // Only the unnamed stream is the file's own size.
            let stream_name = buffer[offset + 9];
            if stream_name == 0 {
                let non_resident = buffer[offset + 8] != 0;
                if non_resident {
                    if offset + 0x38 <= buffer.len() {
                        let allocated = u64::from_le_bytes(
                            buffer[offset + 0x28..offset + 0x30]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        let real = u64::from_le_bytes(
                            buffer[offset + 0x30..offset + 0x38]
                                .try_into()
                                .unwrap_or([0; 8]),
                        );
                        parsed.data_size = Some(if real > 0 { real } else { allocated });
                    }
                } else {
                    parsed.data_size = Some(
                        u32::from_le_bytes(
                            buffer[offset + 0x10..offset + 0x14]
                                .try_into()
                                .unwrap_or([0; 4]),
                        ) as u64,
                    );
                }
            }
        }

        offset += length;
    }

    if parsed.names.is_empty() {
        stats.no_name += 1;
    }

    Some(parsed)
}

/// `$FILE_NAME` value: parent reference, sizes, flags, then the name.
fn parse_file_name(value: &[u8]) -> Option<NameAttr> {
    if value.len() < 0x42 {
        return None;
    }

    let parent = u64::from_le_bytes(value[0..8].try_into().ok()?) & 0x0000_FFFF_FFFF_FFFF;
    let real_size = u64::from_le_bytes(value[0x30..0x38].try_into().ok()?);
    let flags = u32::from_le_bytes(value[0x38..0x3C].try_into().ok()?);
    let name_length = value[0x40] as usize;
    let namespace = value[0x41];

    if name_length == 0 || name_length > 255 || value.len() < 0x42 + name_length * 2 {
        return None;
    }
    // Namespace 2 is the DOS 8.3 alias of the same file.
    if namespace == 2 {
        return None;
    }

    let units: Vec<u16> = value[0x42..0x42 + name_length * 2]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    Some(NameAttr {
        parent,
        name: String::from_utf16_lossy(&units),
        is_dir: flags & 0x1000_0000 != 0,
        reparse: flags & 0x400 != 0,
        size: real_size,
    })
}

/// Turn parsed records into the report the rest of the app expects.
///
/// Files contribute their size to their parent directory; directories form the
/// tree. Records below `FIRST_USER_RECORD` and names starting with `$` are NTFS
/// metadata and are skipped, which is what `robocopy` does too.
pub fn build_report(root: &str, records: &[FileRecord], duration_ms: u128, skipped: u64) -> ScanReport {
    let mut dirs: Vec<&FileRecord> = Vec::new();
    let mut index_of: HashMap<u64, usize> = HashMap::new();

    for record in records {
        // Record 5 is the root directory and lives below the metadata cutoff.
        if (record.record < FIRST_USER_RECORD && record.record != ROOT_RECORD)
            || record.reparse
            || record.name.starts_with('$')
            || SKIP_NAMES.contains(&record.name.to_ascii_lowercase().as_str())
        {
            continue;
        }
        if record.is_dir {
            index_of.insert(record.record, dirs.len());
            dirs.push(record);
        }
    }

    let root_index = dirs
        .iter()
        .position(|record| record.record == ROOT_RECORD)
        .unwrap_or(0);

    let mut bytes: Vec<u64> = vec![0; dirs.len()];
    let mut files: Vec<u64> = vec![0; dirs.len()];

    // Files: their bytes belong to the containing directory.
    for record in records {
        if record.is_dir || record.record < FIRST_USER_RECORD || record.reparse {
            continue;
        }
        if record.name.starts_with('$') {
            continue;
        }
        if let Some(&parent) = index_of.get(&record.parent) {
            bytes[parent] += record.size;
            files[parent] += 1;
        }
    }

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, record) in dirs.iter().enumerate() {
        if index == root_index {
            continue;
        }
        if let Some(&parent) = index_of.get(&record.parent) {
            children.entry(parent).or_default().push(index);
        }
    }

    // Pre-order from the root, carrying the path.
    let mut paths: Vec<String> = vec![String::new(); dirs.len()];
    let mut parent_of: Vec<Option<usize>> = vec![None; dirs.len()];
    let mut order: Vec<usize> = Vec::with_capacity(dirs.len());

    let mut stack: Vec<(usize, String)> = vec![(root_index, root.trim_end_matches('\\').to_string())];
    while let Some((index, parent_path)) = stack.pop() {
        let record = dirs[index];
        let path = if index == root_index {
            root.to_string()
        } else if parent_path.ends_with('\\') {
            // The root already ends with a separator.
            format!("{parent_path}{}", record.name)
        } else {
            format!("{parent_path}\\{}", record.name)
        };

        paths[index] = path.clone();
        order.push(index);

        for &child in children.get(&index).into_iter().flatten() {
            parent_of[child] = Some(index);
            stack.push((child, path.clone()));
        }
    }

    // Reverse pre-order is children-before-parents.
    for &index in order.iter().rev() {
        if let Some(parent) = parent_of[index] {
            bytes[parent] += bytes[index];
            files[parent] += files[index];
        }
    }

    let entries: Vec<DirEntry> = order
        .iter()
        .map(|&index| DirEntry {
            path: paths[index].clone(),
            size: bytes[index],
            files: files[index],
            // The MFT cannot tell us about package.json; the caller fills this
            // in for the handful of folders where it matters.
            has_package_json: false,
        })
        .collect();

    let total_bytes = entries
        .iter()
        .find(|entry| entry.path.trim_end_matches('\\').len() <= 2)
        .map(|entry| entry.size)
        .unwrap_or(0);
    let total_files = entries
        .iter()
        .find(|entry| entry.path.trim_end_matches('\\').len() <= 2)
        .map(|entry| entry.files)
        .unwrap_or(0);

    ScanReport {
        root: root.to_string(),
        dirs: entries,
        total_bytes,
        total_files,
        skipped,
        duration_ms,
    }
}

/// One extent of the MFT: cluster offset and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub lcn: i64,
    pub clusters: u64,
}

/// Parse a non-resident run list: pairs of (length, delta) with variable widths.
pub fn parse_run_list(bytes: &[u8]) -> Vec<Extent> {
    let mut extents = Vec::new();
    let mut previous: i64 = 0;
    let mut at = 0;

    while at < bytes.len() {
        let header = bytes[at];
        if header == 0 {
            break;
        }
        let length_bytes = (header & 0x0F) as usize;
        let offset_bytes = (header >> 4) as usize;
        if length_bytes == 0 || offset_bytes == 0 {
            break;
        }
        at += 1;
        if at + length_bytes + offset_bytes > bytes.len() {
            break;
        }

        let mut clusters: u64 = 0;
        for index in 0..length_bytes {
            clusters |= (bytes[at + index] as u64) << (8 * index);
        }
        at += length_bytes;

        let mut delta: i64 = 0;
        for index in 0..offset_bytes {
            delta |= (bytes[at + index] as i64) << (8 * index);
        }
        // Sign-extend the delta.
        let shift = 64 - 8 * offset_bytes;
        delta = (delta << shift) >> shift;
        at += offset_bytes;

        previous += delta;
        extents.push(Extent {
            lcn: previous,
            clusters,
        });
    }

    extents
}

/// Read the run list of `$MFT`'s unnamed `$DATA` attribute from record 0.
pub fn mft_extents(record_zero: &[u8], bytes_per_sector: u64) -> Vec<Extent> {
    let mut buffer = record_zero.to_vec();
    if !apply_fixup(&mut buffer, bytes_per_sector) {
        return Vec::new();
    }

    let first_attribute = u16::from_le_bytes([buffer[0x14], buffer[0x15]]) as usize;
    let mut offset = first_attribute;

    while offset + 16 <= buffer.len() {
        let kind = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap_or([0; 4]));
        if kind == ATTR_END {
            break;
        }
        let length = u32::from_le_bytes(buffer[offset + 4..offset + 8].try_into().unwrap_or([0; 4]))
            as usize;
        if length < 16 || offset + length > buffer.len() {
            break;
        }

        if kind == ATTR_DATA && buffer[offset + 8] != 0 {
            // Non-resident: run list offset is a u16 at +0x20.
            let run_offset =
                u16::from_le_bytes([buffer[offset + 0x20], buffer[offset + 0x21]]) as usize;
            if run_offset < length {
                return parse_run_list(&buffer[offset + run_offset..offset + length]);
            }
        }

        offset += length;
    }

    Vec::new()
}

/// Read every MFT record from the volume. Requires Administrator.
/// "\\.\C:" is how Windows names a raw volume; the colon is part of the device.
pub fn volume_path(volume: &str) -> String {
    let mount = volume.trim_end_matches('\\');
    format!("\\\\.\\{mount}")
}

pub fn read_records(volume: &str) -> Result<(Vec<FileRecord>, VolumeInfo, u64, u128, ReadStats), String> {
    let mut stats = ReadStats::default();
    let started = Instant::now();

    let path = volume_path(volume);

    let mut file = File::open(&path)
        .map_err(|error| format!("cannot open {path} — {error} (needs Administrator)"))?;

    let mut boot = vec![0u8; 4096];
    file.read_exact(&mut boot)
        .map_err(|error| format!("cannot read boot sector: {error}"))?;
    let info = parse_boot(&boot).ok_or_else(|| "not an NTFS volume".to_string())?;

    // Record 0 describes the MFT itself.
    let mut record_zero = vec![0u8; info.record_size as usize];
    file.seek(SeekFrom::Start(info.mft_lcn * info.bytes_per_cluster))
        .map_err(|error| error.to_string())?;
    file.read_exact(&mut record_zero)
        .map_err(|error| format!("cannot read $MFT record: {error}"))?;

    let extents = mft_extents(&record_zero, info.bytes_per_sector);
    if extents.is_empty() {
        return Err("$MFT has no readable data runs".to_string());
    }

    let mut records: Vec<FileRecord> = Vec::new();
    let mut extensions: HashMap<u64, Vec<NameAttr>> = HashMap::new();
    let mut ext_data: HashMap<u64, u64> = HashMap::new();
    let mut nameless: HashMap<u64, Option<u64>> = HashMap::new();
    let mut produced: std::collections::HashSet<u64> = std::collections::HashSet::new();

    let mut skipped = 0u64;
    let mut record_number = 0u64;
    let record_size = info.record_size as usize;
    let mut carry: Vec<u8> = Vec::new();

    for extent in extents {
        if extent.lcn < 0 {
            skipped += extent.clusters;
            continue;
        }
        let start = extent.lcn as u64 * info.bytes_per_cluster;
        let length = extent.clusters * info.bytes_per_cluster;

        file.seek(SeekFrom::Start(start))
            .map_err(|error| error.to_string())?;

        let mut remaining = length;
        let mut buffer = vec![0u8; 4 * 1024 * 1024];

        while remaining > 0 {
            let want = remaining.min(buffer.len() as u64) as usize;
            let read = file
                .read(&mut buffer[..want])
                .map_err(|error| format!("read failed at record {record_number}: {error}"))?;
            if read == 0 {
                break;
            }
            remaining -= read as u64;
            carry.extend_from_slice(&buffer[..read]);

            let mut at = 0;
            while carry.len() - at >= record_size {
                let chunk = &carry[at..at + record_size];
                if let Some(parsed) = parse_record(chunk, info.bytes_per_sector, &mut stats) {
                    if parsed.base != 0 {
                        // Extension record: its names and sizes belong to the base.
                        if let Some(size) = parsed.data_size {
                            if size > 0 {
                                ext_data.insert(parsed.base, size);
                            }
                        }
                        if !parsed.names.is_empty() {
                            extensions
                                .entry(parsed.base)
                                .or_default()
                                .extend(parsed.names);
                        }
                    } else if parsed.names.is_empty() {
                        nameless.insert(record_number, parsed.data_size);
                    } else {
                        let size_override = parsed.data_size;
                        for name in parsed.names {
                            let size = if name.is_dir {
                                0
                            } else {
                                size_override.unwrap_or(name.size)
                            };
                            records.push(FileRecord {
                                record: record_number,
                                parent: name.parent,
                                name: name.name,
                                is_dir: name.is_dir,
                                size,
                                reparse: name.reparse,
                            });
                        }
                        produced.insert(record_number);
                    }
                }
                at += record_size;
                record_number += 1;
            }
            carry.drain(..at);
        }
    }

    // Records whose name lives in an extension: adopt it now that all of them
    // have been seen.
    for (base, names) in extensions {
        if produced.contains(&base) {
            continue;
        }
        let size_override = nameless.get(&base).copied().flatten();
        for name in names {
            let size = if name.is_dir {
                0
            } else {
                size_override.unwrap_or(name.size)
            };
            records.push(FileRecord {
                record: base,
                parent: name.parent,
                name: name.name,
                is_dir: name.is_dir,
                size,
                reparse: name.reparse,
            });
        }
        produced.insert(base);
    }

    // pagefile.sys and hiberfil.sys keep their $DATA in an extension record, so
    // they arrive here with no size at all.
    for record in records.iter_mut() {
        if record.is_dir || record.size > 0 {
            continue;
        }
        if let Some(size) = ext_data.get(&record.record) {
            record.size = *size;
        }
    }

    stats.parsed = records.len() as u64;
    stats.dirs = records.iter().filter(|record| record.is_dir).count() as u64;
    stats.files = records.iter().filter(|record| !record.is_dir).count() as u64;
    stats.bytes_in_records = records
        .iter()
        .filter(|record| !record.is_dir)
        .map(|record| record.size)
        .sum();

    Ok((
        records,
        info,
        skipped,
        started.elapsed().as_millis(),
        stats,
    ))
}

/// Fill in `has_package_json` for the folders whose rule needs it.
///
/// The MFT has no idea what a package.json is, and one rule (`node_modules`
/// outside an installed app) depends on it. Only folders named `node_modules`
/// are checked, so this costs a few hundred `read_dir` calls, not another walk.
pub fn mark_package_json(entries: &mut [DirEntry]) {
    for entry in entries.iter_mut() {
        let name = entry
            .path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name != "node_modules" {
            continue;
        }

        let Some(parent) = std::path::Path::new(&entry.path).parent() else {
            continue;
        };
        entry.has_package_json = parent.join("package.json").exists();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot_sector() -> Vec<u8> {
        let mut sector = vec![0u8; 512];
        sector[3..11].copy_from_slice(b"NTFS    ");
        sector[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        sector[0x0D] = 8; // 4096-byte clusters
        sector[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
        sector[0x40] = (-10i8) as u8; // 1024-byte records
        sector
    }

    #[test]
    fn parses_the_boot_sector() {
        let info = parse_boot(&boot_sector()).expect("parse");
        assert_eq!(info.bytes_per_sector, 512);
        assert_eq!(info.bytes_per_cluster, 4096);
        assert_eq!(info.mft_lcn, 4);
        assert_eq!(info.record_size, 1024);
    }

    #[test]
    fn rejects_a_volume_that_is_not_ntfs() {
        let mut sector = boot_sector();
        sector[3..11].copy_from_slice(b"FAT32   ");
        assert!(parse_boot(&sector).is_none());
    }

    #[test]
    fn fixup_restores_the_last_bytes_of_each_sector() {
        // Two sectors, USN 0x0001, replacements 0xAAAA and 0xBBBB.
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(&MFT_RECORD_MAGIC.to_le_bytes());
        record[4..6].copy_from_slice(&48u16.to_le_bytes());
        record[6..8].copy_from_slice(&3u16.to_le_bytes());
        record[48..50].copy_from_slice(&1u16.to_le_bytes());
        record[50..52].copy_from_slice(&0xAAAAu16.to_le_bytes());
        record[52..54].copy_from_slice(&0xBBBBu16.to_le_bytes());
        record[510..512].copy_from_slice(&1u16.to_le_bytes());
        record[1022..1024].copy_from_slice(&1u16.to_le_bytes());

        assert!(apply_fixup(&mut record, 512));
        assert_eq!(u16::from_le_bytes([record[510], record[511]]), 0xAAAA);
        assert_eq!(u16::from_le_bytes([record[1022], record[1023]]), 0xBBBB);
    }

    #[test]
    fn a_record_with_a_wrong_fixup_is_rejected_not_parsed() {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(&MFT_RECORD_MAGIC.to_le_bytes());
        record[4..6].copy_from_slice(&48u16.to_le_bytes());
        record[6..8].copy_from_slice(&3u16.to_le_bytes());
        record[48..50].copy_from_slice(&7u16.to_le_bytes());
        record[510..512].copy_from_slice(&9u16.to_le_bytes());

        assert!(!apply_fixup(&mut record, 512));
    }

    #[test]
    fn run_list_walks_signed_deltas() {
        // length 2 at LCN 10, then length 3 at LCN -1 (delta -11)... then 1 at +5.
        let bytes = [0x11, 0x02, 0x0A, 0x11, 0x03, 0xF5, 0x00];
        let extents = parse_run_list(&bytes);

        assert_eq!(extents.len(), 2);
        assert_eq!(extents[0], Extent { lcn: 10, clusters: 2 });
        assert_eq!(extents[1], Extent { lcn: -1, clusters: 3 });
    }

    /// Build a minimal in-use record with a $FILE_NAME and a non-resident $DATA.
    fn build_record(file_name_size: u64, data_size: u64, is_dir: bool) -> Vec<u8> {
        let record_size = 1024usize;
        let mut record = vec![0u8; record_size];

        record[0..4].copy_from_slice(&MFT_RECORD_MAGIC.to_le_bytes());
        record[4..6].copy_from_slice(&48u16.to_le_bytes()); // update sequence offset
        record[6..8].copy_from_slice(&3u16.to_le_bytes()); // 1 + 1024/512
        record[0x14..0x16].copy_from_slice(&56u16.to_le_bytes()); // first attribute
        record[0x16..0x18].copy_from_slice(&0x01u16.to_le_bytes()); // in use

        // --- FILE_NAME (resident)
        let name: Vec<u16> = "node_modules".encode_utf16().collect();
        let value_len = 0x42 + name.len() * 2;
        let attr_len = (24 + value_len + 7) & !7;
        let mut at = 56;

        record[at..at + 4].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        record[at + 4..at + 8].copy_from_slice(&(attr_len as u32).to_le_bytes());
        record[at + 0x10..at + 0x14].copy_from_slice(&(value_len as u32).to_le_bytes());
        record[at + 0x14..at + 0x16].copy_from_slice(&24u16.to_le_bytes());

        let value = at + 24;
        record[value..value + 8].copy_from_slice(&ROOT_RECORD.to_le_bytes());
        record[value + 0x30..value + 0x38].copy_from_slice(&file_name_size.to_le_bytes());
        let flags: u32 = if is_dir { 0x1000_0000 } else { 0x80 };
        record[value + 0x38..value + 0x3C].copy_from_slice(&flags.to_le_bytes());
        record[value + 0x40] = name.len() as u8;
        record[value + 0x41] = 1; // Win32 namespace
        for (index, unit) in name.iter().enumerate() {
            let at = value + 0x42 + index * 2;
            record[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }

        // --- DATA (non-resident), real size at +0x30
        at += attr_len;
        let data_len = 0x48usize;
        record[at..at + 4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        record[at + 4..at + 8].copy_from_slice(&(data_len as u32).to_le_bytes());
        record[at + 8] = 1; // non-resident
        record[at + 0x20..at + 0x22].copy_from_slice(&0x40u16.to_le_bytes()); // run offset
        record[at + 0x30..at + 0x38].copy_from_slice(&data_size.to_le_bytes());
        record[at + 0x40] = 0x11; // one-byte length, one-byte offset
        record[at + 0x41] = 0x01;
        record[at + 0x42] = 0x10;
        record[at + 0x43] = 0x00; // end of run list

        at += data_len;
        record[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());

        // --- fixup, applied last so it covers the finished buffer
        record[48..50].copy_from_slice(&1u16.to_le_bytes());
        for sector in 1..3u16 {
            let position = sector as usize * 512 - 2;
            record[50 + (sector as usize - 1) * 2..52 + (sector as usize - 1) * 2]
                .copy_from_slice(&0xBEEFu16.to_le_bytes());
            record[position..position + 2].copy_from_slice(&1u16.to_le_bytes());
        }

        record
    }

    #[test]
    fn the_data_stream_size_wins_over_the_name_copy() {
        // Real case: a 24 GB file whose $FILE_NAME size field reads zero.
        let record = build_record(0, 24 * 1024 * 1024 * 1024, false);
        let mut stats = ReadStats::default();
        let parsed = parse_record(&record, 512, &mut stats).expect("parse");

        assert_eq!(parsed.names.len(), 1);
        assert_eq!(parsed.names[0].name, "node_modules");
        assert!(!parsed.names[0].is_dir);
        assert_eq!(parsed.data_size, Some(24 * 1024 * 1024 * 1024));
        assert_eq!(parsed.names[0].size, 0, "the name copy says zero");
    }

    #[test]
    fn a_directory_is_flagged_from_its_name_attribute() {
        let record = build_record(0, 4096, true);
        let mut stats = ReadStats::default();
        let parsed = parse_record(&record, 512, &mut stats).expect("parse");

        assert!(parsed.names[0].is_dir);
    }

    #[test]
    fn the_volume_device_path_is_well_formed() {
        assert_eq!(volume_path("C:\\"), "\\\\.\\C:");
        assert_eq!(volume_path("D:"), "\\\\.\\D:");
    }
    fn dir(record: u64, parent: u64, name: &str) -> FileRecord {
        FileRecord {
            record,
            parent,
            name: name.to_string(),
            is_dir: true,
            size: 0,
            reparse: false,
        }
    }

    fn file(record: u64, parent: u64, name: &str, size: u64) -> FileRecord {
        FileRecord {
            record,
            parent,
            name: name.to_string(),
            is_dir: false,
            size,
            reparse: false,
        }
    }

    #[test]
    fn sizes_roll_up_from_files_through_directories() {
        // root(5) -> Users(30) -> you(31) -> { a.txt 100, b.txt 200 }
        //          -> Windows(32) -> { c.sys 50 }
        let records = vec![
            dir(ROOT_RECORD, ROOT_RECORD, "."),
            dir(30, ROOT_RECORD, "Users"),
            dir(31, 30, "you"),
            dir(32, ROOT_RECORD, "Windows"),
            file(40, 31, "a.txt", 100),
            file(41, 31, "b.txt", 200),
            file(42, 32, "c.sys", 50),
        ];

        let report = build_report("C:\\", &records, 1, 0);
        let find = |path: &str| {
            report
                .dirs
                .iter()
                .find(|entry| entry.path == path)
                .unwrap_or_else(|| panic!("missing {path}"))
        };

        assert_eq!(find("C:\\").size, 350);
        assert_eq!(find("C:\\Users").size, 300);
        assert_eq!(find("C:\\Users\\you").size, 300);
        assert_eq!(find("C:\\Users\\you").files, 2);
        assert_eq!(find("C:\\Windows").size, 50);
        assert_eq!(report.total_bytes, 350);
        assert_eq!(report.total_files, 3);
    }

    #[test]
    fn metadata_and_reparse_records_are_skipped() {
        let records = vec![
            dir(ROOT_RECORD, ROOT_RECORD, "."),
            dir(3, ROOT_RECORD, "$MFT"),
            FileRecord {
                reparse: true,
                ..dir(33, ROOT_RECORD, "junction")
            },
            file(43, ROOT_RECORD, "real.bin", 10),
        ];

        let report = build_report("C:\\", &records, 1, 0);
        let names: Vec<&str> = report.dirs.iter().map(|entry| entry.path.as_str()).collect();

        assert_eq!(names.len(), 1, "only the root survives: {names:?}");
        assert_eq!(report.total_bytes, 10, "the junction must not be counted");
    }

    #[test]
    fn a_cycle_does_not_hang() {
        // Parent pointers that loop: the DFS must still terminate.
        let records = vec![
            dir(ROOT_RECORD, ROOT_RECORD, "."),
            dir(50, 51, "a"),
            dir(51, 50, "b"),
        ];

        let report = build_report("C:\\", &records, 1, 0);
        assert!(report.dirs.len() <= 3);
    }
}
