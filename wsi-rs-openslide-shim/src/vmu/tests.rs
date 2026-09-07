use super::*;
use wsi_rs::{AxesShape, CpuTileData, DatasetId, LevelIdx, RegionRequest, SceneId, SeriesId};

struct NativeRgb12 {
    dataset: Dataset,
}

impl SlideReader for NativeRgb12 {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }
    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        CpuTile::new(
            2,
            2,
            3,
            ColorSpace::Rgb,
            CpuTileLayout::Interleaved,
            CpuTileData::u16([15, 16, 4095].repeat(4)),
        )
    }
    fn read_associated(&self, _name: &str) -> Result<CpuTile, WsiError> {
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10, 20, 30])
    }
}

#[test]
fn native_precision_is_converted_before_fractional_composition() {
    let level = Level::new(
        (2, 2),
        1.0,
        TileLayout::Regular {
            tile_width: 2,
            tile_height: 2,
            tiles_across: 1,
            tiles_down: 1,
        },
    );
    let series = Series::new(
        "rgb",
        AxesShape::default(),
        vec![level],
        SampleType::Uint16,
        vec![],
    );
    let mut dataset = Dataset::new(
        DatasetId::new(1),
        vec![Scene::new("vmu", vec![series]).with_name("synthetic")],
    );
    dataset.properties.insert("openslide.vendor", "hamamatsu");
    dataset.properties.insert("hamamatsu.BitsPerPixel", "36");
    dataset.properties.insert("hamamatsu.PixelOrder", "RGB");
    let native = Slide::from_source_with_cache_bytes(Box::new(NativeRgb12 { dataset }), 0);
    let display = display_slide(native).unwrap();
    assert_eq!(
        display.dataset().scenes[0].series[0].sample_type,
        SampleType::Uint8
    );
    assert_eq!(
        display.dataset().scenes[0].name.as_deref(),
        Some("synthetic")
    );
    let request = RegionRequest::new(
        SceneId::new(0),
        SeriesId::new(0),
        LevelIdx::new(0),
        (0, 0),
        (1, 1),
    );
    for offset in [(0.0, 0.0), (0.5, 0.5)] {
        let rgba = display
            .read_region_subpixel(&request, offset)
            .unwrap()
            .into_rgba()
            .unwrap();
        assert_eq!(rgba.as_raw(), &[0, 1, 255, 255]);
    }
    assert_eq!(
        display.read_associated("macro").unwrap().as_u8().unwrap(),
        &[10, 20, 30]
    );
}
