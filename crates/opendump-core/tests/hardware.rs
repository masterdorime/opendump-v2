use opendump_core::hardware::{memory, parse_nvidia_smi};

#[test]
fn parses_an_nvidia_row_into_bytes() {
    let gpu = parse_nvidia_smi("NVIDIA GeForce RTX 4060 Laptop GPU, 8188, 7565\n").expect("parse");

    assert_eq!(gpu.name, "NVIDIA GeForce RTX 4060 Laptop GPU");
    assert_eq!(gpu.vram_total, 8188 * 1024 * 1024);
    assert_eq!(gpu.vram_free, 7565 * 1024 * 1024);
}

#[test]
fn ignores_leading_blank_lines() {
    let gpu = parse_nvidia_smi("\r\n\r\nNVIDIA GeForce RTX 3060, 12288, 12000").expect("parse");
    assert_eq!(gpu.vram_total, 12288 * 1024 * 1024);
}

#[test]
fn rejects_a_non_nvidia_adapter() {
    // These may report plausible numbers that do not mean the same thing.
    assert!(parse_nvidia_smi("AMD Radeon RX 7600, 8192, 8000").is_none());
    assert!(parse_nvidia_smi("Intel(R) UHD Graphics, 128, 100").is_none());
}

#[test]
fn rejects_free_memory_above_total() {
    assert!(parse_nvidia_smi("NVIDIA GeForce RTX 3060, 4096, 5000").is_none());
}

#[test]
fn rejects_garbage_and_unavailable_fields() {
    assert!(parse_nvidia_smi("").is_none());
    assert!(parse_nvidia_smi("   \n\n").is_none());
    assert!(parse_nvidia_smi("No devices were found").is_none());
    assert!(parse_nvidia_smi("NVIDIA GeForce RTX 3060, [N/A], [N/A]").is_none());
    assert!(parse_nvidia_smi("NVIDIA GeForce RTX 3060, 0, 0").is_none());
    assert!(parse_nvidia_smi("NVIDIA GeForce RTX 3060, 4096").is_none());
}

#[test]
fn memory_reports_physical_ram() {
    let memory = memory();

    assert!(memory.ram_total > 0, "no RAM reported");
    assert!(memory.ram_free <= memory.ram_total);
    // Not asserting `ram_free > 0`: a machine under extreme pressure may not.
}
