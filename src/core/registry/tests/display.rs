use super::super::*;
use super::support::MockSource;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn read_display_tile_regular_native_passthrough() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let buf = handle
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 1,
            row: 0,
            tile_width: 256,
            tile_height: 256,
        })
        .unwrap();
    assert_eq!(buf.width, 256);
    assert_eq!(buf.height, 256);
    let data = buf.data.as_u8().unwrap();
    assert_eq!(&data[..3], &[0, 255, 0]);
}

#[test]
fn read_display_tile_composes_subtile_from_regular_grid() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let buf = handle
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
            tile_width: 128,
            tile_height: 128,
        })
        .unwrap();
    assert_eq!(buf.width, 128);
    assert_eq!(buf.height, 128);
    let data = buf.data.as_u8().unwrap();
    assert_eq!(&data[..3], &[255, 0, 0]);
}

/// Mock source with a non-256-aligned level (300x260) to test edge tile
/// origin calculation. Each pixel encodes its level-space x coordinate in
/// the red channel so we can verify the tile was read from the right origin.
struct EdgeMockSource {
    ds: Dataset,
}

impl EdgeMockSource {
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
                            dimensions: (300, 260),
                            downsample: 1.0,
                            tile_layout: TileLayout::Regular {
                                tile_width: 256,
                                tile_height: 256,
                                tiles_across: 2,
                                tiles_down: 2,
                            },
                        }],
                        sample_type: SampleType::Uint8,
                        channels: vec![],
                    }],
                }],
                associated_images: HashMap::new(),
                properties: crate::Properties::new(),
                icc_profiles: HashMap::new(),
                source_icc_profiles: Vec::new(),
            },
        }
    }
}

impl SlideReader for EdgeMockSource {
    fn dataset(&self) -> &Dataset {
        &self.ds
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        // Return native 256x256 tiles with pixel R = (tile_origin_x + px) & 0xFF
        let tile_origin_x = req.col as u32 * 256;
        let level_w = 300u32;
        let tile_w = 256.min(level_w.saturating_sub(tile_origin_x));
        let tile_h = 256.min(260u32.saturating_sub(req.row as u32 * 256));
        let mut data = vec![0u8; (tile_w * tile_h * 3) as usize];
        for y in 0..tile_h {
            for x in 0..tile_w {
                let idx = ((y * tile_w + x) * 3) as usize;
                let abs_x = tile_origin_x + x;
                data[idx] = (abs_x & 0xFF) as u8; // R = level-space x
                data[idx + 1] = (y & 0xFF) as u8; // G = local y
                data[idx + 2] = 42;
            }
        }
        Ok(CpuTile {
            width: tile_w,
            height: tile_h,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(data),
        })
    }
    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

#[test]
fn display_tile_edge_origin_correct_with_full_tile_width() {
    // Level is 300x260. With 256x256 grid, last column (col=1) starts at
    // x=256 and has content_width=44. Passing tile_width=256 must produce
    // an origin of 256 (not col*content_width=1*44=44).
    let source: Box<dyn SlideReader> = Box::new(EdgeMockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache);

    let buf = handle
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 1,
            row: 0,
            tile_width: 256,
            tile_height: 256,
        })
        .unwrap();

    // The edge tile should be clipped to 44x256.
    assert_eq!(buf.width, 44);
    assert_eq!(buf.height, 256);

    // First pixel should be from level-space x=256, not x=44.
    let data = buf.data.as_u8().unwrap();
    let first_r = data[0];
    assert_eq!(
        first_r,
        (256u32 & 0xFF) as u8,
        "edge tile first pixel R should encode level-space x=256, got x={}",
        first_r,
    );
}
