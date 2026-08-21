use super::*;
use crate::Engine;

fn manifest_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}

#[test]
fn pinned_version_check_fails_closed() {
    assert!(validate_version("4.0.1", Some("4.0.1")).is_ok());
    assert!(validate_version("4.0.1-custom", Some("4.0.1")).is_ok());
    assert!(validate_version("4.0.0", Some("4.0.1")).is_err());
    assert!(validate_version("anything", None).is_ok());
}

#[test]
fn read_checksum_includes_coordinates_and_argb_bytes() {
    let spec = ReadSpec {
        x: 10,
        y: 20,
        level: 1,
        width: 2,
        height: 1,
    };
    let digest = |spec, pixels: &[u32]| {
        let mut checksum = Sha256::new();
        hash_read(&mut checksum, spec, pixels);
        format!("{:x}", checksum.finalize())
    };

    let first = digest(spec, &[0x1122_3344, 0x5566_7788]);

    assert_eq!(first, digest(spec, &[0x1122_3344, 0x5566_7788]));
    assert_ne!(
        first,
        digest(ReadSpec { x: 11, ..spec }, &[0x1122_3344, 0x5566_7788])
    );
    assert_ne!(first, digest(spec, &[0x1122_3344, 0x5566_7789]));
}

#[test]
fn files_are_canonicalized_and_hashed_deterministically() {
    let manifest = manifest_path();
    let canonical = canonical_file(&manifest, "manifest").expect("canonical manifest");
    assert!(canonical.is_absolute());

    let first = sha256_file(&manifest).expect("manifest hash");
    let second = sha256_file(&manifest).expect("manifest hash");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);

    let missing = Path::new("definitely-missing-perf-runner-input");
    assert!(canonical_file(missing, "fixture")
        .unwrap_err()
        .contains("not a file"));
    assert!(sha256_file(missing).unwrap_err().contains("failed to open"));
}

#[test]
fn workload_accounting_covers_cache_shares_buffers_and_checksums() {
    assert_eq!(cache_share(10, 3, 0), 4);
    assert_eq!(cache_share(10, 3, 1), 3);
    assert_eq!(cache_share(10, 3, 2), 3);

    let spec = ReadSpec {
        x: 1,
        y: 2,
        level: 0,
        width: 3,
        height: 2,
    };
    let mut buffer = vec![1; 20];
    prepare_buffer(spec, &mut buffer).expect("buffer");
    assert_eq!(buffer, vec![1; 6]);
    assert_ne!(
        read_digest(spec, &buffer),
        read_digest(ReadSpec { x: 2, ..spec }, &buffer)
    );

    let mut checksum = Sha256::new();
    hash_levels(
        &mut checksum,
        &[LevelInfo {
            width: 3,
            height: 2,
            downsample: 1.0,
        }],
    );
    let first =
        finish_workload("first", vec![10, 20, 30], 3_000, 2, 30, checksum).expect("workload");
    assert_eq!(first.n, 3);
    assert_eq!(first.throughput_bytes_per_second, 100_000_000);

    let zero_elapsed =
        finish_workload("zero", vec![1], 100, 1, 0, Sha256::new()).expect("zero-elapsed workload");
    assert_eq!(zero_elapsed.throughput_bytes_per_second, 0);
    assert!(finish_workload("empty", vec![], 0, 1, 0, Sha256::new()).is_err());
    assert_ne!(first.checksum_sha256, zero_elapsed.checksum_sha256);

    let level = LevelResult::from(LevelInfo {
        width: 5,
        height: 4,
        downsample: 2.0,
    });
    assert_eq!((level.width, level.height, level.downsample), (5, 4, 2.0));
    assert!(elapsed_micros(Instant::now()) <= 1_000_000);
}

#[test]
fn orchestration_fails_with_context_before_or_during_dynamic_loading() {
    let missing = Path::new("definitely-missing-perf-input").to_path_buf();
    let manifest = manifest_path();
    let config = |library_path, slide_path| WorkerConfig {
        engine: Engine::WsiRs,
        library_path,
        slide_path,
        repeat_index: 0,
        cache_bytes: 1_024,
        workers: 2,
        only: Some("single_tile_l0".into()),
        required_version_prefix: None,
    };

    assert!(run(&config(missing.clone(), manifest.clone()))
        .unwrap_err()
        .contains("OpenSlide library is not a file"));
    assert!(run(&config(manifest.clone(), missing))
        .unwrap_err()
        .contains("slide is not a file"));
    assert!(run(&config(manifest.clone(), manifest.clone()))
        .unwrap_err()
        .contains("failed to load"));

    let workload = Workload {
        name: "test_reads",
        warmup: true,
        reads: vec![
            ReadSpec {
                x: 0,
                y: 0,
                level: 0,
                width: 1,
                height: 1,
            },
            ReadSpec {
                x: 1,
                y: 1,
                level: 0,
                width: 1,
                height: 1,
            },
        ],
    };
    assert!(run_read_workload(&manifest, &manifest, 1_024, 2, workload)
        .unwrap_err()
        .contains("failed to load"));
}
