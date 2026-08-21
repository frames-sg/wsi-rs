use super::*;

#[test]
fn ndpi_fixup_low_offset_unchanged() {
    // When both diroff and offset are in the low 4GB, result should be offset
    let result = fix_offset_ndpi(1000, 500);
    assert_eq!(result, 500);
}

#[test]
fn ndpi_fixup_high_diroff_reconstructs() {
    // diroff at 5GB, offset stored as low 32 bits of 4.5GB
    let diroff: u64 = 5 * 1024 * 1024 * 1024; // 5 GB
    let real_offset: u64 = 4 * 1024 * 1024 * 1024 + 500_000_000; // 4.5 GB
    let stored_offset = real_offset & u64::from(u32::MAX); // low 32 bits
    let result = fix_offset_ndpi(diroff, stored_offset);
    assert_eq!(result, real_offset);
}

#[test]
fn ndpi_fixup_result_below_diroff() {
    // The fixup should always produce a result <= diroff
    // (data referenced by an IFD should precede it)
    let diroff: u64 = 6 * 1024 * 1024 * 1024;
    let stored_offset: u64 = 100;
    let result = fix_offset_ndpi(diroff, stored_offset);
    assert!(result <= diroff, "result {} > diroff {}", result, diroff);
}

#[test]
fn ndpi_fixup_zero_diroff() {
    // When diroff is 0, the heuristic clamps: result >= diroff triggers
    // saturating_sub(4GB) which floors to 0.
    let result = fix_offset_ndpi(0, 12345);
    assert_eq!(result, 0);
}

#[test]
fn opens_wrapped_first_ifd_ndpi_when_corpus_is_available() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path = workspace_root.join("downloads/openslide-testdata/Hamamatsu/Hamamatsu-1.ndpi");
    if !path.exists() {
        return;
    }

    let container = TiffContainer::open(&path).expect("open wrapped-offset NDPI");
    assert!(container.is_ndpi());
    assert!(!container.top_ifds().is_empty());
}

// ── SubIFD test helpers ───────────────────────────────────
