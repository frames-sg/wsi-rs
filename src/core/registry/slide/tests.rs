use super::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};

struct TinySource {
    dataset: Dataset,
}

impl TinySource {
    fn new() -> Self {
        Self {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(77),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (1, 1),
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
        }
    }
}

impl SlideReader for TinySource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10, 20, 30])
    }
}

#[test]
fn slide_cache_poison_recovery_keeps_public_inspection_and_conversion_working() {
    let slide = Arc::new(Slide::from_source(
        Box::new(TinySource::new()),
        Arc::new(TileCache::new(4096)),
    ));
    assert!(format!("{slide:?}").contains("dataset_id"));
    assert_eq!(
        slide.decode_execution_options(),
        DecodeExecutionOptions::default()
    );

    let poisoned = Arc::clone(&slide);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.cache.write().unwrap();
        panic!("poison slide cache owner");
    })
    .join();

    let replacement = Arc::new(TileCache::new(8192));
    let detached = slide.replace_shared_tile_cache(Arc::clone(&replacement));
    assert!(!Arc::ptr_eq(&detached, &replacement));

    let tile = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    assert!(!slide.cached_tile_present(&tile));
    let view = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 1, 1);
    assert!(matches!(
        slide.read_raw_compressed_display_tile(&view),
        Err(WsiError::Unsupported { .. })
    ));

    let region = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));
    let rgba = slide
        .read_region_rgba_windowed(&region, &DisplayWindow::new(0.0, 255.0).unwrap())
        .expect("windowed convenience conversion");
    assert_eq!(rgba.get_pixel(0, 0).0, [10, 20, 30, 255]);
}

#[test]
fn subpixel_region_rejects_offsets_outside_one_pixel() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    for offset in [(-0.1, 0.0), (1.0, 0.0), (0.0, f64::NAN)] {
        let err = slide
            .read_region_subpixel(&req, offset)
            .expect_err("invalid subpixel offset must fail");
        assert!(
            err.to_string()
                .contains("subpixel offset must be finite and in [0, 1)"),
            "unexpected error for {offset:?}: {err}"
        );
    }
}

#[test]
fn zero_subpixel_offset_preserves_integral_region_path() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let integral = slide.read_region(&req).expect("integral region");
    let subpixel = slide
        .read_region_subpixel(&req, (0.0, 0.0))
        .expect("zero-offset region");

    assert_eq!(subpixel.data.as_u8(), integral.data.as_u8());
}

#[test]
fn fractional_region_preserves_filter_coverage_as_rgba() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let tile = slide
        .read_region_subpixel(&req, (0.5, 0.5))
        .expect("fractional region");
    let pixel = tile.as_u8().expect("RGBA8 fractional region");

    assert_eq!(tile.color_space(), &ColorSpace::Rgba);
    assert_eq!(tile.channels(), 4);
    assert_eq!(pixel, &[8, 20, 28, 64]);
}

#[test]
fn tiny_nonzero_subpixel_offset_still_uses_fractional_composition() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let tile = slide
        .read_region_subpixel(&req, (f64::EPSILON, f64::EPSILON))
        .expect("tiny fractional region");

    assert_eq!(tile.color_space(), &ColorSpace::Rgba);
}
