use super::super::*;
use super::support::{CountingSource, MockSource};
use crate::properties::Properties;
use crate::test_support::{region_request, regular_rgb_dataset_for_test, RegularLevelForTest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct GrayscaleSource {
    ds: Dataset,
}

impl GrayscaleSource {
    fn new() -> Self {
        Self {
            ds: Dataset {
                id: DatasetId::new(2),
                scenes: vec![Scene {
                    id: "s0".into(),
                    name: None,
                    series: vec![Series {
                        id: "ser0".into(),
                        axes: AxesShape::default(),
                        levels: vec![Level {
                            dimensions: (128, 128),
                            downsample: 1.0,
                            tile_layout: TileLayout::Regular {
                                tile_width: 128,
                                tile_height: 128,
                                tiles_across: 1,
                                tiles_down: 1,
                            },
                        }],
                        sample_type: SampleType::Uint16,
                        channels: vec![ChannelInfo {
                            name: Some("Gray".into()),
                            color: None,
                            excitation_nm: None,
                            emission_nm: None,
                        }],
                    }],
                }],
                associated_images: HashMap::new(),
                properties: Properties::new(),
                icc_profiles: HashMap::new(),
                source_icc_profiles: Vec::new(),
            },
        }
    }
}

impl SlideReader for GrayscaleSource {
    fn dataset(&self) -> &Dataset {
        &self.ds
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        Ok(CpuTile {
            width: 128,
            height: 128,
            channels: 1,
            color_space: ColorSpace::Grayscale,
            layout: CpuTileLayout::Planar,
            data: CpuTileData::u16(vec![7u16; 128 * 128]),
        })
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

struct GridReader {
    ds: Dataset,
}

impl GridReader {
    fn new() -> Self {
        let level = Level {
            dimensions: (8, 8),
            downsample: 1.0,
            tile_layout: TileLayout::Regular {
                tile_width: 2,
                tile_height: 2,
                tiles_across: 4,
                tiles_down: 4,
            },
        };
        Self {
            ds: Dataset {
                id: DatasetId::new(99),
                scenes: vec![Scene {
                    id: "scene".into(),
                    name: None,
                    series: vec![Series {
                        id: "series".into(),
                        axes: AxesShape::default(),
                        levels: vec![level],
                        sample_type: SampleType::Uint8,
                        channels: vec![
                            ChannelInfo {
                                name: None,
                                color: None,
                                excitation_nm: None,
                                emission_nm: None,
                            };
                            3
                        ],
                    }],
                }],
                associated_images: HashMap::new(),
                properties: Properties::new(),
                icc_profiles: HashMap::new(),
                source_icc_profiles: Vec::new(),
            },
        }
    }
}

impl SlideReader for GridReader {
    fn dataset(&self) -> &Dataset {
        &self.ds
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        Ok(reqs
            .iter()
            .map(|req| {
                let mut bytes = vec![0u8; 2 * 2 * 3];
                for pixel in bytes.chunks_exact_mut(3) {
                    pixel[0] = (req.col & 0xff) as u8;
                    pixel[1] = (req.row & 0xff) as u8;
                }
                CpuTile::from_u8_interleaved(2, 2, 3, ColorSpace::Rgb, bytes).unwrap()
            })
            .collect())
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        unimplemented!("GridReader tests exercise batch-primary read_region")
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

#[test]
fn read_region_default_composes_across_tile_boundary() {
    let reader = GridReader::new();
    let req = RegionRequest {
        scene: SceneId::new(0),
        series: SeriesId::new(0),
        level: LevelIdx::new(0),
        plane: PlaneIdx::default(),
        origin_px: (1, 1),
        size_px: (4, 4),
    };
    let cpu = reader.read_region(&req).expect("read region");
    assert_eq!((cpu.width, cpu.height), (4, 4));
    let bytes = cpu.data.as_u8().unwrap();
    assert_eq!(&bytes[0..3], &[0, 0, 0]);
    assert_eq!(&bytes[3..6], &[1, 0, 0]);
    assert_eq!(&bytes[12..15], &[0, 1, 0]);
}

#[test]
fn compositor_respects_custom_max_region_pixels_before_tile_reads() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let source = CountingSource::new(DatasetId::new(55), tile_reads.clone());
    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 11, 10);

    let err = composite_region_from_source(&source, None, &req, 100).unwrap_err();

    assert!(
        err.to_string().contains("exceeds maximum of 100 pixels"),
        "unexpected error: {err}"
    );
    assert_eq!(tile_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn compositor_checks_custom_max_region_pixels_before_empty_region_zero_fill() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let source = CountingSource::new(DatasetId::new(56), tile_reads.clone());
    let req = region_request(0, 0, 0, PlaneSelection::default(), 10_000, 10_000, 11, 10);

    let err = composite_region_from_source(&source, None, &req, 100).unwrap_err();

    assert!(
        err.to_string().contains("exceeds maximum of 100 pixels"),
        "unexpected error: {err}"
    );
    assert_eq!(tile_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn read_region_single_tile() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 100, 100);
    let buf = handle.read_region(&req).unwrap();
    assert_eq!(buf.width, 100);
    assert_eq!(buf.height, 100);
    assert_eq!(buf.channels, 3);
    assert_eq!(buf.color_space, ColorSpace::Rgb);

    // All pixels should be red (tile 0,0)
    let data = buf.data.as_u8().unwrap();
    assert_eq!(data[0], 255); // R
    assert_eq!(data[1], 0); // G
    assert_eq!(data[2], 0); // B
                            // Check last pixel too
    let last = (100 * 100 - 1) * 3;
    assert_eq!(data[last], 255);
    assert_eq!(data[last + 1], 0);
    assert_eq!(data[last + 2], 0);
}

#[test]
fn read_region_multi_tile_compositing() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    // Request spanning all four tiles: full 512x512
    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 512, 512);
    let buf = handle.read_region(&req).unwrap();
    assert_eq!(buf.width, 512);
    assert_eq!(buf.height, 512);

    let data = buf.data.as_u8().unwrap();

    // Top-left pixel (0,0) -> tile (0,0) -> red
    assert_eq!(&data[0..3], &[255, 0, 0]);

    // Top-right pixel (511,0) -> tile (1,0) -> green
    let idx = 511 * 3;
    assert_eq!(&data[idx..idx + 3], &[0, 255, 0]);

    // Bottom-left pixel (0,511) -> tile (0,1) -> blue
    let idx = (511 * 512) * 3;
    assert_eq!(&data[idx..idx + 3], &[0, 0, 255]);

    // Bottom-right pixel (511,511) -> tile (1,1) -> white
    let idx = (511 * 512 + 511) * 3;
    assert_eq!(&data[idx..idx + 3], &[255, 255, 255]);
}

#[test]
fn read_region_cross_tile_boundary() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    // 2x2 region crossing the tile boundary at x=256
    let req = region_request(0, 0, 0, PlaneSelection::default(), 255, 0, 2, 1);
    let buf = handle.read_region(&req).unwrap();
    let data = buf.data.as_u8().unwrap();

    // Pixel at x=255 -> tile (0,0) -> red
    assert_eq!(&data[0..3], &[255, 0, 0]);
    // Pixel at x=256 -> tile (1,0) -> green
    assert_eq!(&data[3..6], &[0, 255, 0]);
}

#[test]
fn read_region_partially_outside_level_keeps_zero_fill() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection::default(), -1, 0, 2, 1);
    let buf = handle.read_region(&req).unwrap();
    let data = buf.data.as_u8().unwrap();

    assert_eq!(&data[0..3], &[0, 0, 0]);
    assert_eq!(&data[3..6], &[255, 0, 0]);
}

#[test]
fn read_region_scene_out_of_range() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(5, 0, 0, PlaneSelection::default(), 0, 0, 10, 10);
    match handle.read_region(&req) {
        Err(WsiError::SceneOutOfRange { index: 5, count: 1 }) => {}
        other => panic!("expected SceneOutOfRange, got {:?}", other),
    }
}

#[test]
fn read_region_series_out_of_range() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let handle = Slide::from_source(source, Arc::new(TileCache::new(1024)));
    let req = region_request(0, 7, 0, PlaneSelection::default(), 0, 0, 10, 10);

    match handle.read_region(&req) {
        Err(WsiError::SeriesOutOfRange { index: 7, count: 1 }) => {}
        other => panic!("expected SeriesOutOfRange, got {other:?}"),
    }
}

#[test]
fn read_region_level_out_of_range() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 99, PlaneSelection::default(), 0, 0, 10, 10);
    match handle.read_region(&req) {
        Err(WsiError::LevelOutOfRange {
            level: 99,
            count: 1,
        }) => {}
        other => panic!("expected LevelOutOfRange, got {:?}", other),
    }
}

#[test]
fn read_region_plane_out_of_range() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection { z: 5, c: 0, t: 0 }, 0, 0, 10, 10);
    match handle.read_region(&req) {
        Err(WsiError::PlaneOutOfRange {
            axis,
            value: 5,
            max: 1,
        }) => {
            assert_eq!(axis, "z");
        }
        other => panic!("expected PlaneOutOfRange, got {:?}", other),
    }
}

#[test]
fn read_region_channel_and_time_planes_are_validated_independently() {
    for (plane, expected_axis) in [
        (PlaneSelection { z: 0, c: 3, t: 0 }, "c"),
        (PlaneSelection { z: 0, c: 0, t: 4 }, "t"),
    ] {
        let source: Box<dyn SlideReader> = Box::new(MockSource::new());
        let handle = Slide::from_source(source, Arc::new(TileCache::new(1024)));
        let req = region_request(0, 0, 0, plane, 0, 0, 10, 10);

        match handle.read_region(&req) {
            Err(WsiError::PlaneOutOfRange { axis, max: 1, .. }) => {
                assert_eq!(axis, expected_axis);
            }
            other => panic!("expected PlaneOutOfRange for {expected_axis}, got {other:?}"),
        }
    }
}

#[test]
fn read_region_no_tiles_hit_returns_zeros() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(1024));
    let handle = Slide::from_source(source, cache);

    // Region entirely outside the level (level is 512x512)
    let req = region_request(0, 0, 0, PlaneSelection::default(), 10000, 10000, 10, 10);
    let buf = handle.read_region(&req).unwrap();
    assert_eq!(buf.width, 10);
    assert_eq!(buf.height, 10);
    // All zeros
    let data = buf.data.as_u8().unwrap();
    assert!(data.iter().all(|&b| b == 0));
}

#[test]
fn read_region_no_tiles_hit_preserves_template_metadata() {
    let source: Box<dyn SlideReader> = Box::new(GrayscaleSource::new());
    let cache = Arc::new(TileCache::new(1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection::default(), 512, 512, 16, 16);
    let buf = handle.read_region(&req).unwrap();

    assert_eq!(buf.channels, 1);
    assert_eq!(buf.color_space, ColorSpace::Grayscale);
    assert_eq!(buf.layout, CpuTileLayout::Planar);
    assert_eq!(buf.data.sample_type(), SampleType::Uint16);
    assert!(buf.data.as_u16().unwrap().iter().all(|sample| *sample == 0));
}

struct FailingTileSource {
    ds: Dataset,
}

impl FailingTileSource {
    fn new() -> Self {
        Self {
            ds: regular_rgb_dataset_for_test(
                DatasetId::new(9),
                "s0",
                "ser0",
                RegularLevelForTest {
                    dimensions: (128, 128),
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
        }
    }
}

impl SlideReader for FailingTileSource {
    fn dataset(&self) -> &Dataset {
        &self.ds
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        Err(WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "synthetic decode failure".into(),
        })
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

#[test]
fn read_region_no_tiles_hit_falls_back_when_probe_tile_read_fails() {
    let source: Box<dyn SlideReader> = Box::new(FailingTileSource::new());
    let cache = Arc::new(TileCache::new(1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection::default(), 512, 512, 16, 16);
    let buf = handle.read_region(&req).unwrap();

    assert_eq!(buf.channels, 3);
    assert_eq!(buf.color_space, ColorSpace::Rgb);
    assert_eq!(buf.layout, CpuTileLayout::Interleaved);
    assert!(buf.data.as_u8().unwrap().iter().all(|sample| *sample == 0));
}

#[test]
fn read_region_rgba_produces_correct_image() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 256, 256);
    let img = handle.read_region_rgba(&req).unwrap();
    assert_eq!(img.width(), 256);
    assert_eq!(img.height(), 256);

    // All pixels should be red with full alpha (tile 0,0)
    let pixel = img.get_pixel(0, 0);
    assert_eq!(pixel.0, [255, 0, 0, 255]);

    let pixel = img.get_pixel(255, 255);
    assert_eq!(pixel.0, [255, 0, 0, 255]);
}

#[test]
fn read_region_rgba_multi_tile() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 512, 512);
    let img = handle.read_region_rgba(&req).unwrap();
    assert_eq!(img.width(), 512);
    assert_eq!(img.height(), 512);

    // Top-left -> red
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    // Top-right -> green
    assert_eq!(img.get_pixel(511, 0).0, [0, 255, 0, 255]);
    // Bottom-left -> blue
    assert_eq!(img.get_pixel(0, 511).0, [0, 0, 255, 255]);
    // Bottom-right -> white
    assert_eq!(img.get_pixel(511, 511).0, [255, 255, 255, 255]);
}
