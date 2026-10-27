//! Hardware probe.
//!
//! RAM comes from the OS. VRAM comes from `nvidia-smi`, because it is the only
//! source that reports **free** video memory — and free VRAM is what decides
//! whether a model fits. WMI's `AdapterRAM` is a 32-bit field that reports an
//! 8 GB card as 4 GB, so reading it would recommend a model that then spills.
//!
//! When no GPU can be queried the fields are `None`. That is deliberate: v0.5
//! shipped the same rule after a guessed number caused a bad recommendation.

use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub name: String,
    pub vram_total: u64,
    pub vram_free: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Memory {
    pub ram_total: u64,
    pub ram_free: u64,
    /// `None` when no GPU could be queried — never a guess.
    pub gpu: Option<Gpu>,
}

/// Parse one `nvidia-smi --query-gpu=name,memory.total,memory.free
/// --format=csv,noheader,nounits` row. Values are MiB.
pub fn parse_nvidia_smi(output: &str) -> Option<Gpu> {
    let line = output.lines().find(|line| !line.trim().is_empty())?;
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }

    // An AMD or Intel adapter can answer with numbers that mean something else,
    // so the name has to look like the vendor we know how to read.
    let name = parts[0];
    let lower = name.to_ascii_lowercase();
    const VENDORS: [&str; 7] = [
        "nvidia", "geforce", "rtx", "gtx", "quadro", "tesla", "titan",
    ];
    if !VENDORS.iter().any(|vendor| lower.contains(vendor)) {
        return None;
    }

    let total_mib: u64 = parts[1].parse().ok()?;
    let free_mib: u64 = parts[2].parse().ok()?;
    if total_mib == 0 || free_mib > total_mib {
        return None;
    }

    Some(Gpu {
        name: name.to_string(),
        vram_total: total_mib * MIB,
        vram_free: free_mib * MIB,
    })
}

/// Physical memory, right now.
#[cfg(windows)]
pub fn memory() -> Memory {
    #[repr(C)]
    struct StatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut StatusEx) -> i32;
    }

    let mut status = StatusEx {
        length: std::mem::size_of::<StatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };

    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Memory::default();
    }

    Memory {
        ram_total: status.total_phys,
        ram_free: status.avail_phys,
        gpu: None,
    }
}

#[cfg(not(windows))]
pub fn memory() -> Memory {
    Memory::default()
}

/// Ask the NVIDIA driver for VRAM, capped at four seconds.
///
/// Blocking: call it off the UI thread. `nvidia-smi` is a 100–200 ms subprocess
/// on a healthy machine, but a wedged driver must never hang the caller.
#[cfg(windows)]
pub fn query_gpu() -> Option<Gpu> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut child = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;

    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(40));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    let output = child.wait_with_output().ok()?;
    parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(windows))]
pub fn query_gpu() -> Option<Gpu> {
    None
}
