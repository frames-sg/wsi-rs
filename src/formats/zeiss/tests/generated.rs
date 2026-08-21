use super::fixtures::*;
use crate::core::registry::{DatasetReader, FormatProbe, ProbeConfidence, SlideReader};
use crate::core::types::{RegionRequest, TileOutputPreference, TileRequest, TileViewRequest};
use crate::formats::zeiss::slide::{ZeissReader, ZeissSlide};
use crate::formats::zeiss::{ZeissBackend, FILE_MAGIC};
use crate::WsiError;
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

#[test]
fn probe_distinguishes_missing_short_wrong_and_valid_czi_files() {
    let backend = ZeissBackend;
    let directory = tempfile::tempdir().expect("probe directory");
    let missing = backend
        .probe(&directory.path().join("missing.czi"))
        .expect("missing probe result");
    assert!(!missing.detected);
    assert!(missing.vendor.is_empty());

    for bytes in [b"short".as_slice(), b"not-a-czi-header!".as_slice()] {
        let mut file = tempfile::NamedTempFile::new().expect("probe fixture");
        file.write_all(bytes).expect("write probe fixture");
        let result = backend.probe(file.path()).expect("probe result");
        assert!(!result.detected);
    }

    let fixture = main_fixture();
    let result = backend.probe(fixture.path()).expect("valid probe result");
    assert!(result.detected);
    assert_eq!(result.vendor, "zeiss");
    assert!(matches!(result.confidence, ProbeConfidence::Definite));
}

#[test]
fn generated_czi_opens_with_metadata_levels_and_regions() {
    let fixture = main_fixture();
    let reader = ZeissBackend
        .open(fixture.path())
        .expect("open generated CZI");
    let dataset = reader.dataset();
    assert_eq!(dataset.scenes.len(), 1);
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 2);
    assert_eq!(series.levels[0].dimensions, (4, 2));
    assert_eq!(series.levels[1].dimensions, (2, 1));
    assert_eq!(series.channels.len(), 1);
    assert_eq!(series.channels[0].name.as_deref(), Some("Brightfield"));
    assert_eq!(series.channels[0].color, Some([0x11, 0x22, 0x33]));
    assert_eq!(dataset.properties.get("openslide.vendor"), Some("zeiss"));
    assert_eq!(dataset.properties.get("openslide.mpp-x"), Some("0.250000"));
    assert_eq!(dataset.properties.get("openslide.mpp-y"), Some("0.500000"));
    assert_eq!(
        dataset.properties.get("openslide.objective-power"),
        Some("40")
    );
    assert_eq!(dataset.properties.get("openslide.region[0].x"), Some("0"));
    assert_eq!(dataset.properties.get("openslide.region[0].y"), Some("0"));
    assert_eq!(
        dataset.properties.get("openslide.region[0].width"),
        Some("4")
    );
    assert_eq!(
        dataset.properties.get("openslide.region[0].height"),
        Some("2")
    );
    assert_eq!(dataset.associated_images["label"].dimensions, (2, 1));
    assert_eq!(dataset.associated_images["macro"].dimensions, (1, 1));
    assert_eq!(
        dataset
            .properties
            .get("openslide.quickhash-1")
            .expect("quickhash property")
            .len(),
        64
    );
}

#[test]
fn generated_czi_reads_tiles_batches_levels_and_cache_hits() {
    let fixture = main_fixture();
    let slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI");
    let composed = slide
        .scene_level_image(0, 0)
        .expect("compose full-resolution level");
    assert_eq!((composed.width, composed.height), (4, 2));
    assert_eq!(
        composed.data.as_u8(),
        Some(
            [
                1, 2, 3, 4, 5, 6, 13, 14, 15, 16, 17, 18, 7, 8, 9, 10, 11, 12, 19, 20, 21, 22, 23,
                24,
            ]
            .as_slice()
        )
    );
    let reduced = slide
        .scene_level_image(0, 1)
        .expect("compose reduced level");
    assert_eq!(
        reduced.data.as_u8(),
        Some([28, 29, 30, 31, 32, 33].as_slice())
    );
    assert_eq!(
        slide
            .scene_level_image(0, 1)
            .expect("reuse composed level")
            .data
            .as_u8(),
        reduced.data.as_u8()
    );

    let reader = ZeissBackend
        .open(fixture.path())
        .expect("open generated CZI");
    let level_zero = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let tile = reader
        .read_tile_cpu(&level_zero)
        .expect("read level-zero tile");
    assert_eq!((tile.width, tile.height, tile.channels), (4, 2, 3));
    assert_eq!(
        tile.data.as_u8().expect("u8 RGB"),
        &[1, 2, 3, 4, 5, 6, 13, 14, 15, 16, 17, 18, 7, 8, 9, 10, 11, 12, 19, 20, 21, 22, 23, 24,]
    );
    let cached = reader.read_tile_cpu(&level_zero).expect("read cached tile");
    assert_eq!(cached.data.as_u8(), tile.data.as_u8());

    let level_one = TileRequest::new(0usize, 0usize, 1u32, 0, 0);
    let downsampled = reader.read_tile_cpu(&level_one).expect("read reduced tile");
    assert_eq!((downsampled.width, downsampled.height), (2, 1));
    assert_eq!(
        downsampled.data.as_u8(),
        Some([28, 29, 30, 31, 32, 33].as_slice())
    );

    let batch = reader
        .read_tiles(
            &[level_zero.clone(), level_one],
            TileOutputPreference::cpu_only(),
        )
        .expect("read mixed-level batch");
    assert_eq!(batch.len(), 2);
    let error = reader
        .read_tiles(&[level_zero], TileOutputPreference::require_device_auto())
        .expect_err("Zeiss cannot return device-resident tiles");
    assert!(error.to_string().contains("RequireDevice"));

    let poisoned_slide =
        Arc::new(ZeissSlide::parse(fixture.path()).expect("parse CZI for poisoned-cache recovery"));
    let poison_target = poisoned_slide.clone();
    assert!(std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _guard = poison_target
            .associated_cache
            .lock()
            .expect("unpoisoned associated cache");
        panic!("poison associated cache for recovery test");
    }))
    .is_err());
    let poison_target = poisoned_slide.clone();
    assert!(std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _guard = poison_target
            .tile_cache
            .lock()
            .expect("unpoisoned tile cache");
        panic!("poison tile cache for recovery test");
    }))
    .is_err());
    let poison_target = poisoned_slide.clone();
    assert!(std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _guard = poison_target
            .level_cache
            .lock()
            .expect("unpoisoned level cache");
        panic!("poison level cache for recovery test");
    }))
    .is_err());
    let poison_target = poisoned_slide.clone();
    assert!(std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _guard = poison_target.czi.lock().expect("unpoisoned CZI reader");
        panic!("poison CZI reader for recovery test");
    }))
    .is_err());
    let recovery_slide = poisoned_slide.clone();
    let poisoned_reader = ZeissReader {
        slide: poisoned_slide,
    };
    let recovered = poisoned_reader
        .read_associated("label")
        .expect("poisoned private caches recover");
    assert_eq!((recovered.width, recovered.height), (2, 1));
    let recovered = poisoned_reader
        .read_tile_cpu(&TileRequest::new(0usize, 0usize, 0u32, 0, 0))
        .expect("poisoned tile cache recovers");
    assert_eq!((recovered.width, recovered.height), (4, 2));
    let recovered = recovery_slide
        .scene_level_image(0, 0)
        .expect("poisoned level cache recovers");
    assert_eq!((recovered.width, recovered.height), (4, 2));
}

#[test]
fn generated_czi_public_region_display_and_associated_reads_are_deterministic() {
    let fixture = main_fixture();
    let slide = crate::Slide::open(fixture.path()).expect("open public CZI slide");
    let region = slide
        .read_region(&RegionRequest::new(0usize, 0usize, 0u32, (1, 0), (3, 2)))
        .expect("read generated CZI region");
    assert_eq!((region.width, region.height), (3, 2));
    assert_eq!(
        region.data.as_u8(),
        Some([4, 5, 6, 13, 14, 15, 16, 17, 18, 10, 11, 12, 19, 20, 21, 22, 23, 24,].as_slice())
    );

    let display = slide
        .read_display_tile(&TileViewRequest::new(0usize, 0usize, 0u32, 1, 0, 2, 1))
        .expect("read generated CZI display tile");
    assert_eq!((display.width, display.height), (2, 1));
    assert_eq!(
        display.data.as_u8(),
        Some([13, 14, 15, 16, 17, 18].as_slice())
    );

    let label = slide.read_associated("label").expect("read label JPEG");
    assert_eq!((label.width, label.height, label.channels), (2, 1, 3));
    let cached_label = slide.read_associated("label").expect("read cached label");
    assert_eq!(cached_label.data.as_u8(), label.data.as_u8());
    let macro_image = slide.read_associated("macro").expect("read embedded CZI");
    assert_eq!((macro_image.width, macro_image.height), (1, 1));
    assert_eq!(macro_image.data.as_u8(), Some([9, 8, 7].as_slice()));
    assert!(matches!(
        slide.read_associated("thumbnail"),
        Err(WsiError::AssociatedImageNotFound(name)) if name == "thumbnail"
    ));
}

#[test]
fn generated_czi_reports_index_bounds_and_corrupt_input_context() {
    let fixture = main_fixture();
    let reader = ZeissBackend
        .open(fixture.path())
        .expect("open generated CZI");
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(1usize, 0usize, 0u32, 0, 0)),
        Err(WsiError::SceneOutOfRange { index: 1, count: 1 })
    ));
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(0usize, 1usize, 0u32, 0, 0)),
        Err(WsiError::SceneOutOfRange { index: 0, count: 1 })
    ));
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(0usize, 0usize, 9u32, 0, 0)),
        Err(WsiError::LevelOutOfRange { level: 9, count: 2 })
    ));
    for (col, row) in [(-1, 0), (0, -1), (1, 0), (0, 1)] {
        let error = reader
            .read_tile_cpu(&TileRequest::new(0usize, 0usize, 0u32, col, row))
            .expect_err("out-of-range tile must fail");
        assert!(matches!(error, WsiError::TileRead { .. }));
    }

    let mut truncated = build_czi_bytes(
        &[SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3])],
        &[],
        &metadata_xml(1, 1),
    );
    truncated.truncate(64);
    let mut file = tempfile::Builder::new()
        .suffix(".czi")
        .tempfile()
        .expect("truncated fixture");
    file.write_all(&truncated).expect("write truncated fixture");
    let error = match ZeissBackend.open(file.path()) {
        Ok(_) => panic!("truncated CZI unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, WsiError::InvalidSlide { .. }));
    assert!(error.to_string().contains("file header"));

    let mut wrong_magic = vec![0; 544];
    wrong_magic[..FILE_MAGIC.len()].copy_from_slice(FILE_MAGIC);
    wrong_magic[0] = b'X';
    let mut file = tempfile::Builder::new()
        .suffix(".czi")
        .tempfile()
        .expect("wrong-magic fixture");
    file.write_all(&wrong_magic)
        .expect("write wrong-magic fixture");
    let error = match ZeissBackend.open(file.path()) {
        Ok(_) => panic!("wrong-magic CZI unexpectedly opened"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("magic"));
}
