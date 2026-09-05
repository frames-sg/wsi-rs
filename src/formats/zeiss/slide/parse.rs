use super::super::attachments::{associated_name, probe_associated_attachment};
use super::super::metadata::*;
use super::super::preflight::preflight_czi_file_with_limits;
use super::super::*;

struct ValidatedCzi {
    czi: CziFile,
    preflight_file: File,
    source_identity: FileIdentity,
    header: czi_rs::FileHeaderInfo,
    xml: String,
    summary: czi_rs::MetadataSummary,
    statistics: czi_rs::SubBlockStatistics,
    attachments: Vec<czi_rs::AttachmentInfo>,
    subblocks: Vec<czi_rs::DirectorySubBlockInfo>,
}

struct CziDatasetParts {
    czi: CziFile,
    preflight_file: File,
    source_identity: FileIdentity,
    attachments: Vec<czi_rs::AttachmentInfo>,
    dataset_id: DatasetId,
    scenes: Vec<Scene>,
    properties: Properties,
    subblock_origin: (i32, i32),
    canvas_level_subblocks: Vec<Vec<usize>>,
    canvas_level_tile_subblocks: Vec<CanvasTileSubblockMap>,
}

struct CziAssociatedImages {
    metadata: HashMap<String, AssociatedImage>,
    sources: HashMap<String, czi_rs::AttachmentInfo>,
}

impl ZeissSlide {
    pub(in crate::formats::zeiss) fn parse_with_cache_config(
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Self, WsiError> {
        Self::parse_with_config(
            path,
            BackendOpenConfig::new(cache_config, crate::SlideLimits::default()),
        )
    }

    pub(in crate::formats::zeiss) fn parse_with_config(
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Self, WsiError> {
        let validated = open_and_validate_czi(path, config.limits)?;
        let mut parts = build_dataset_parts(path, validated, config.limits)?;
        let associated =
            probe_associated_images(path, &mut parts.czi, &parts.attachments, config.limits)?;
        let mut slide = assemble_slide(path, config.cache_config, parts, associated);
        slide.limits = config.limits;
        Ok(slide)
    }
}

fn open_and_validate_czi(
    path: &Path,
    limits: crate::SlideLimits,
) -> Result<ValidatedCzi, WsiError> {
    let source_identity = preflight_czi_file_with_limits(path, limits)?;
    let mut preflight_file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    if FileIdentity::from_open_file(path, &preflight_file)? != source_identity {
        return Err(invalid_slide(
            path,
            "CZI source identity changed before subblock preflight",
        ));
    }
    let mut czi =
        CziFile::open(path).map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
    if FileIdentity::from_path(path)? != source_identity {
        return Err(invalid_slide(
            path,
            "CZI source identity changed between preflight and parser open",
        ));
    }

    if czi.subblocks().len() > MAX_CZI_SUBBLOCKS {
        return Err(invalid_slide(
            path,
            format!(
                "CZI subblock count {} exceeds the {MAX_CZI_SUBBLOCKS}-entry safety limit",
                czi.subblocks().len()
            ),
        ));
    }
    if czi.attachments().len() > MAX_CZI_ATTACHMENTS {
        return Err(invalid_slide(
            path,
            format!(
                "CZI attachment count {} exceeds the {MAX_CZI_ATTACHMENTS}-entry safety limit",
                czi.attachments().len()
            ),
        ));
    }
    for subblock in czi.subblocks() {
        checked_product_to_usize(
            &[
                u64::from(subblock.stored_size.w),
                u64::from(subblock.stored_size.h),
                u64::try_from(subblock.pixel_type.bytes_per_pixel()).unwrap_or(u64::MAX),
            ],
            MAX_DECODED_IMAGE_BYTES.min(limits.decoded_output_bytes()),
            "CZI decoded subblock",
        )
        .map_err(|message| invalid_slide(path, message))?;
    }
    let file_len = std::fs::metadata(path)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?
        .len();
    for attachment in czi.attachments() {
        if attachment.data_size > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
            return Err(invalid_slide(
                path,
                format!(
                    "CZI attachment {} declares {} bytes, exceeding the {}-byte safety limit",
                    attachment.name,
                    attachment.data_size,
                    crate::core::limits::MAX_COMPRESSED_INPUT_BYTES
                ),
            ));
        }
        let payload_end = attachment
            .file_position
            .checked_add(32 + 256)
            .and_then(|offset| offset.checked_add(attachment.data_size))
            .ok_or_else(|| invalid_slide(path, "CZI attachment range overflows"))?;
        if payload_end > file_len {
            return Err(invalid_slide(
                path,
                format!(
                    "CZI attachment {} ends at {payload_end}, beyond file length {file_len}",
                    attachment.name
                ),
            ));
        }
    }

    for subblock in czi.subblocks() {
        super::super::preflight::preflight_czi_open_subblock_with_limits(
            path,
            &mut preflight_file,
            subblock.file_position,
            limits,
        )?;
        if subblock.file_part != 0 {
            return Err(invalid_slide(path, "multipart CZI is unsupported"));
        }
    }
    let header = czi.file_header().clone();
    let xml = czi
        .metadata_xml()
        .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
    if xml.len() as u64 > MAX_CZI_METADATA_BYTES {
        return Err(invalid_slide(
            path,
            format!(
                "CZI metadata has {} bytes, exceeding the {MAX_CZI_METADATA_BYTES}-byte safety limit",
                xml.len()
            ),
        ));
    }
    let xml = xml.to_string();
    let summary = czi
        .metadata()
        .map_err(|source| WsiError::DisplayConversion(source.to_string()))?
        .clone();

    for dimension in CziDimension::FRAME_ORDER
        .into_iter()
        .filter(|&d| d != CziDimension::S)
    {
        if summary
            .image
            .sizes
            .get(&dimension)
            .is_some_and(|&size| size > 1)
            || czi
                .statistics()
                .dim_bounds
                .get(dimension)
                .is_some_and(|range| range.size > 1)
        {
            return Err(WsiError::UnsupportedFormat(
                "CZI WSI reads currently require single-plane images".into(),
            ));
        }
    }
    Ok(ValidatedCzi {
        header,
        xml,
        summary,
        statistics: czi.statistics().clone(),
        attachments: czi.attachments().to_vec(),
        subblocks: czi.subblocks().to_vec(),
        source_identity,
        preflight_file,
        czi,
    })
}

fn build_dataset_parts(
    path: &Path,
    mut input: ValidatedCzi,
    limits: crate::SlideLimits,
) -> Result<CziDatasetParts, WsiError> {
    let scene_indices = scene_indices(&input.statistics, &input.summary)?;
    if scene_indices.is_empty() {
        return Err(invalid_slide(path, "Zeiss slide has no scenes"));
    }

    let level_ratios = common_level_ratios(&input.subblocks, &scene_indices, &input.statistics)?;
    if level_ratios.len() > MAX_CZI_LEVELS {
        return Err(invalid_slide(
            path,
            format!(
                "CZI level count {} exceeds the {MAX_CZI_LEVELS}-level safety limit",
                level_ratios.len()
            ),
        ));
    }
    let canvas_origin = canvas_origin(&input.statistics);
    let subblock_origin = subblock_origin(&input.subblocks);
    let canvas_dimensions = canvas_dimensions(&input.statistics, &input.summary, path)?;
    if canvas_dimensions.0 == 0
        || canvas_dimensions.1 == 0
        || canvas_dimensions.0 > u64::from(u32::MAX)
        || canvas_dimensions.1 > u64::from(u32::MAX)
    {
        return Err(invalid_slide(
            path,
            format!(
                "CZI canvas dimensions {}x{} are outside the supported nonzero u32 range",
                canvas_dimensions.0, canvas_dimensions.1
            ),
        ));
    }
    let levels = build_levels(canvas_dimensions, &level_ratios);
    let mut canvas_level_subblocks = vec![Vec::new(); level_ratios.len()];
    for subblock in &input.subblocks {
        if !subblock_matches_default_plane(subblock, &input.statistics) {
            continue;
        }
        let Some(level_ratio) = subblock_ratio(subblock) else {
            continue;
        };
        let Some(level_slot) = level_ratios.iter().position(|ratio| *ratio == level_ratio) else {
            continue;
        };
        canvas_level_subblocks[level_slot].push(subblock.index);
    }
    let budget = crate::core::registry::OpenBudget::new(limits);
    budget.retain_index((input.subblocks.len() as u64).saturating_mul(512))?;
    let canvas_level_tile_subblocks = build_canvas_level_tile_subblocks_with_budget(
        &input.subblocks,
        &canvas_level_subblocks,
        &levels,
        subblock_origin,
        &budget,
    )?;
    let scenes = vec![Scene {
        id: "scene_0".to_string(),
        name: Some("Canvas".to_string()),
        series: vec![Series {
            id: "series_0".to_string(),
            axes: AxesShape::default(),
            levels,
            sample_type: SampleType::Uint8,
            channels: build_channels(&input.summary),
        }],
    }];

    let quickhash = quickhash_for_zeiss(&input.header, &input.xml)?;
    let dataset_id = dataset_id_from_quickhash(path, &quickhash, "quickhash")?;
    let properties = build_properties(
        &mut input.summary,
        &input.xml,
        &input.statistics,
        &scene_indices,
        canvas_origin,
        &quickhash,
    );

    Ok(CziDatasetParts {
        czi: input.czi,
        preflight_file: input.preflight_file,
        source_identity: input.source_identity,
        attachments: input.attachments,
        dataset_id,
        scenes,
        properties,
        subblock_origin,
        canvas_level_subblocks,
        canvas_level_tile_subblocks,
    })
}

fn build_properties(
    summary: &mut czi_rs::MetadataSummary,
    xml: &str,
    statistics: &czi_rs::SubBlockStatistics,
    scene_indices: &[usize],
    canvas_origin: (i32, i32),
    quickhash: &str,
) -> Properties {
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "zeiss");
    properties.insert("openslide.quickhash-1", quickhash);
    if let Some(value) = summary.document.user_name.take() {
        properties.insert("zeiss.document.user_name", value);
    }
    if let Some(value) = summary.document.creation_date.take() {
        properties.insert("zeiss.document.creation_date", value);
    }
    if let Some(value) = summary.document.application_name.take() {
        properties.insert("zeiss.document.application_name", value);
    }
    if let Some(value) = summary.document.application_version.take() {
        properties.insert("zeiss.document.application_version", value);
    }
    if let Some(value) = summary.image.pixel_type.take() {
        properties.insert("zeiss.image.pixel_type", value.as_str());
    }
    if let Some(x) = summary.image.sizes.get(&CziDimension::X) {
        properties.insert("zeiss.image.size_x", x.to_string());
    }
    if let Some(y) = summary.image.sizes.get(&CziDimension::Y) {
        properties.insert("zeiss.image.size_y", y.to_string());
    }
    if let Some(s) = summary.image.sizes.get(&CziDimension::S) {
        properties.insert("zeiss.image.size_s", s.to_string());
    }
    for (mpp_key, scaling_key, value) in [
        ("openslide.mpp-x", "zeiss.scaling.x", summary.scaling.x),
        ("openslide.mpp-y", "zeiss.scaling.y", summary.scaling.y),
    ] {
        if let Some(value) = value {
            properties.insert(mpp_key, format!("{:.6}", value * 1_000_000.0));
            properties.insert(scaling_key, value.to_string());
        }
    }
    if let Some(objective) = extract_objective_magnification(xml) {
        properties.insert("openslide.objective-power", objective);
    }

    for (index, scene_index) in scene_indices.iter().enumerate() {
        if let Some(bounding_boxes) = statistics.scene_bounding_boxes.get(&(*scene_index as i32)) {
            let region = if bounding_boxes.layer0.is_valid() {
                bounding_boxes.layer0
            } else {
                bounding_boxes.all
            };
            if region.is_valid() {
                for (field, value) in [
                    ("x", region.x - canvas_origin.0),
                    ("y", region.y - canvas_origin.1),
                    ("width", region.w),
                    ("height", region.h),
                ] {
                    properties.insert(
                        format!("openslide.region[{index}].{field}"),
                        value.to_string(),
                    );
                }
            }
        }
    }
    properties
}

fn probe_associated_images(
    path: &Path,
    czi: &mut CziFile,
    attachments: &[czi_rs::AttachmentInfo],
    limits: crate::SlideLimits,
) -> Result<CziAssociatedImages, WsiError> {
    let mut associated_images = HashMap::new();
    let mut associated_sources = HashMap::new();
    for attachment in attachments {
        let Some(name) = associated_name(&attachment.name) else {
            continue;
        };
        if let Some(metadata) = probe_associated_attachment(path, czi, attachment, limits)? {
            associated_images.insert(name.to_string(), metadata);
            associated_sources.insert(name.to_string(), attachment.clone());
        }
    }
    Ok(CziAssociatedImages {
        metadata: associated_images,
        sources: associated_sources,
    })
}

fn assemble_slide(
    path: &Path,
    cache_config: CacheConfig,
    parts: CziDatasetParts,
    associated: CziAssociatedImages,
) -> ZeissSlide {
    let dataset = Dataset {
        id: parts.dataset_id,
        scenes: parts.scenes,
        associated_images: associated.metadata,
        properties: parts.properties,
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    };

    let (tile_entry_bytes, level_entry_bytes) = dataset
        .scenes
        .first()
        .and_then(|scene| scene.series.first())
        .map(|series| {
            let level_entry_bytes = series
                .levels
                .iter()
                .map(|level| {
                    level
                        .dimensions
                        .0
                        .saturating_mul(level.dimensions.1)
                        .saturating_mul(3)
                })
                .max()
                .unwrap_or(1);
            let tile_entry_bytes = series
                .levels
                .iter()
                .filter_map(|level| match level.tile_layout {
                    TileLayout::Regular {
                        tile_width,
                        tile_height,
                        ..
                    } => Some(
                        u64::from(tile_width)
                            .saturating_mul(u64::from(tile_height))
                            .saturating_mul(3),
                    ),
                    TileLayout::Irregular { .. } | TileLayout::WholeLevel { .. } => None,
                })
                .max()
                .unwrap_or(1);
            (tile_entry_bytes, level_entry_bytes)
        })
        .unwrap_or((1, 1));
    let associated_entry_bytes = dataset
        .associated_images
        .values()
        .map(|image| {
            u64::from(image.dimensions.0)
                .saturating_mul(u64::from(image.dimensions.1))
                .saturating_mul(u64::from(image.channels))
        })
        .max()
        .unwrap_or(1);

    // Full-resolution Zeiss blocks can be ~14.5 MiB. Reserve half of the same
    // aggregate budget for source reuse, rather than raising the memory cap.
    let mut private_cache_budget = cache_config.private_cache_budget(6);
    let subblock_cache = PrivateCache::new(private_cache_budget.allocate_shares(3));
    let level_cache = PrivateCache::new(private_cache_budget.allocate(level_entry_bytes));
    let tile_cache = PrivateCache::new(private_cache_budget.allocate(tile_entry_bytes));
    let associated_cache = PrivateCache::new(private_cache_budget.allocate(associated_entry_bytes));

    ZeissSlide {
        #[cfg(test)]
        subblock_decodes: AtomicU64::new(0),
        limits: crate::SlideLimits::default(),
        source_path: path.to_path_buf(),
        source_identity: parts.source_identity,
        dataset,
        czi: Mutex::new(parts.czi),
        preflight_file: Mutex::new(parts.preflight_file),
        level_cache: Mutex::new(level_cache),
        tile_cache: Mutex::new(tile_cache),
        subblock_cache: Mutex::new(subblock_cache),
        associated_cache: Mutex::new(associated_cache),
        associated_sources: associated.sources,
        subblock_origin: parts.subblock_origin,
        canvas_level_subblocks: parts.canvas_level_subblocks,
        canvas_level_tile_subblocks: parts.canvas_level_tile_subblocks,
    }
}
