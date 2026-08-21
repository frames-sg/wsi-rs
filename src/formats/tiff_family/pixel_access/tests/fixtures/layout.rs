use super::super::*;

pub(in super::super) fn whole_level(
    dimensions: (u64, u64),
    downsample: f64,
    virtual_tile: (u32, u32),
) -> Level {
    Level {
        dimensions,
        downsample,
        tile_layout: TileLayout::WholeLevel {
            width: dimensions.0,
            height: dimensions.1,
            virtual_tile_width: virtual_tile.0,
            virtual_tile_height: virtual_tile.1,
        },
    }
}

pub(in super::super) fn regular_level(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
) -> Level {
    Level {
        dimensions: (u64::from(width), u64::from(height)),
        downsample: 1.0,
        tile_layout: TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across: u64::from(width.div_ceil(tile_width)),
            tiles_down: u64::from(height.div_ceil(tile_height)),
        },
    }
}

pub(in super::super) fn tile_source_key(level: u32) -> TileSourceKey {
    TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level,
        z: 0,
        c: 0,
        t: 0,
    }
}

pub(in super::super) fn single_series_dataset(
    dataset_id: DatasetId,
    levels: Vec<Level>,
) -> Dataset {
    Dataset {
        id: dataset_id,
        scenes: vec![Scene {
            id: "s0".into(),
            name: None,
            series: vec![Series {
                id: "ser0".into(),
                axes: AxesShape::default(),
                levels,
                sample_type: SampleType::Uint8,
                channels: vec![],
            }],
        }],
        associated_images: HashMap::new(),
        properties: Properties::new(),
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

pub(in super::super) fn single_series_layout(
    dataset_id: DatasetId,
    levels: Vec<Level>,
    tile_sources: HashMap<TileSourceKey, TileSource>,
) -> DatasetLayout {
    DatasetLayout {
        dataset: single_series_dataset(dataset_id, levels),
        tile_sources,
        associated_sources: HashMap::new(),
    }
}

pub(in super::super) fn associated_image_layout(
    dataset_id: DatasetId,
    image_name: &str,
    dimensions: (u32, u32),
    channels: u16,
    source: TileSource,
) -> DatasetLayout {
    DatasetLayout {
        dataset: Dataset {
            id: dataset_id,
            scenes: vec![],
            associated_images: HashMap::from([(
                image_name.to_string(),
                AssociatedImage::new(dimensions, SampleType::Uint8, channels),
            )]),
            properties: Properties::new(),
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        },
        tile_sources: HashMap::new(),
        associated_sources: HashMap::from([(image_name.to_string(), source)]),
    }
}

pub(in super::super) fn stripped_associated_source(
    container: &TiffContainer,
    ifd_id: IfdId,
    compression: Compression,
) -> TileSource {
    TileSource::Stripped {
        ifd_id,
        jpeg_tables: None,
        compression,
        strip_offsets: vec![container.get_u64(ifd_id, tags::STRIP_OFFSETS).unwrap()],
        strip_byte_counts: vec![container.get_u64(ifd_id, tags::STRIP_BYTE_COUNTS).unwrap()],
    }
}
