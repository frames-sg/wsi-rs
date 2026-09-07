//! Convert VMU tiles before Pixman-compatible composition, preserving native U16
//! reads in the Rust API while matching OpenSlide's RGB12 display contract.
use std::sync::Arc;
use wsi_rs::{
    ColorSpace, CpuTile, CpuTileLayout, Dataset, Level, SampleType, Scene, Series, Slide,
    SlideReader, TileCache, TileLayout, TileRequest, WsiError,
};

pub(crate) fn display_slide(native: Slide) -> Result<Slide, WsiError> {
    let source = native.dataset();
    if source.properties.get("openslide.vendor") != Some("hamamatsu")
        || source.properties.get("hamamatsu.BitsPerPixel") != Some("36")
        || source.properties.get("hamamatsu.PixelOrder") != Some("RGB")
    {
        return Ok(native);
    }
    let mut scenes = Vec::new();
    for scene in &source.scenes {
        let mut series_list = Vec::new();
        for series in &scene.series {
            let mut levels = Vec::new();
            for level in &series.levels {
                let TileLayout::Regular {
                    tile_width,
                    tile_height,
                    tiles_across,
                    tiles_down,
                } = level.tile_layout
                else {
                    return Err(WsiError::DisplayConversion(
                        "VMU display requires a regular tile grid".into(),
                    ));
                };
                levels.push(Level::new(
                    level.dimensions,
                    level.downsample,
                    TileLayout::Regular {
                        tile_width,
                        tile_height,
                        tiles_across,
                        tiles_down,
                    },
                ));
            }
            series_list.push(Series::new(
                &series.id,
                series.axes,
                levels,
                SampleType::Uint8,
                series.channels.clone(),
            ));
        }
        let mut copy = Scene::new(&scene.id, series_list);
        copy.name = scene.name.clone();
        scenes.push(copy);
    }
    let mut dataset = Dataset::new(source.id, scenes)
        .with_properties(source.properties.clone())
        .with_associated_images(source.associated_images.clone())
        .with_icc_profiles(source.icc_profiles.clone());
    dataset.source_icc_profiles = source.source_icc_profiles.clone();
    // Only cache display tiles. The native slide has not read pixels yet, so the
    // transferred cache cannot contain RGB16 entries under the same dataset id.
    let cache = native.replace_shared_tile_cache(Arc::new(TileCache::new(0)));
    let slide = Slide::from_source_with_cache_bytes(Box::new(VmuDisplay { native, dataset }), 0);
    slide.replace_shared_tile_cache(cache);
    Ok(slide)
}

struct VmuDisplay {
    native: Slide,
    dataset: Dataset,
}

impl SlideReader for VmuDisplay {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let tile = self.native.read_tile(req)?;
        let values = tile
            .data()
            .as_u16()
            .ok_or_else(|| WsiError::DisplayConversion("VMU display requires RGB16".into()))?;
        if tile.color_space() != &ColorSpace::Rgb
            || tile.channels() != 3
            || tile.layout() != CpuTileLayout::Interleaved
        {
            return Err(WsiError::DisplayConversion(
                "VMU display requires interleaved RGB16".into(),
            ));
        }
        // Match OpenSlide: truncate four low bits, then convert to an 8-bit word.
        let bytes = values.iter().map(|value| (value >> 4) as u8).collect();
        CpuTile::from_u8_interleaved(tile.width(), tile.height(), 3, ColorSpace::Rgb, bytes)
    }
    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.native.read_associated(name)
    }
}

#[cfg(test)]
mod tests;
