use super::super::ini::*;
use super::super::levels::{
    base_level_dimensions, expanded_levels, total_tiles_across, total_tiles_down,
};
use super::super::model::{invalid_slide, VmsJpeg, VmsLevel, VmsSlide};
use super::super::*;
use super::vms_image_count;

struct VmsSources {
    group: HashMap<String, String>,
    macro_path: Option<PathBuf>,
    private_cache_budget: PrivateCacheBudget,
    quickhash: String,
    dataset_id: DatasetId,
    base_images: Vec<Arc<VmsJpeg>>,
    map_image: Arc<VmsJpeg>,
    num_cols: u32,
    num_rows: u32,
}

struct VmsDatasetParts {
    macro_path: Option<PathBuf>,
    private_cache_budget: PrivateCacheBudget,
    dataset_id: DatasetId,
    levels: Vec<VmsLevel>,
    dataset_levels: Vec<Level>,
    properties: Properties,
}

impl VmsSlide {
    #[cfg(test)]
    pub(in crate::formats::hamamatsu_vms) fn parse_with_cache_config(
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Self, WsiError> {
        Self::parse_with_config(
            path,
            BackendOpenConfig::new(cache_config, crate::SlideLimits::default()),
        )
    }

    pub(in crate::formats::hamamatsu_vms) fn parse_with_config(
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Self, WsiError> {
        let budget = OpenBudget::new(config.limits);
        let sources = load_vms_sources(path, config.cache_config, budget.as_ref())?;
        let parts = build_levels_and_properties(path, sources)?;
        assemble_slide(parts)
    }
}

fn load_vms_sources(
    path: &Path,
    cache_config: CacheConfig,
    budget: &OpenBudget,
) -> Result<VmsSources, WsiError> {
    let mut ini = parse_vms_ini_with_budget(path, budget)?;
    let group = ini
        .groups
        .remove(GROUP_VMS)
        .ok_or_else(|| invalid_slide(path, "missing [Virtual Microscope Specimen] group"))?;
    let num_cols = parse_u32(path, &group, KEY_NUM_JPEG_COLS)?;
    let num_rows = parse_u32(path, &group, KEY_NUM_JPEG_ROWS)?;
    if num_cols == 0 || num_rows == 0 {
        return Err(invalid_slide(path, "VMS file has no columns or rows"));
    }

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let image_count = vms_image_count(num_cols, num_rows)
        .ok_or_else(|| invalid_slide(path, "VMS JPEG shard count exceeds safety limit"))?;
    let mut private_cache_budget = cache_config.private_cache_budget(image_count.saturating_add(2));
    let image_index_bytes = u64::try_from(image_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(64);
    budget.retain_index(image_index_bytes)?;
    let mut image_paths = Vec::new();
    image_paths
        .try_reserve_exact(image_count)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "tile/frame index",
            requested: image_index_bytes,
            limit: budget.limits().tile_index_bytes(),
        })?;
    image_paths.resize(image_count, None);
    for (key, value) in &group {
        if !key.starts_with(KEY_IMAGE_FILE) {
            continue;
        }
        let dims = parse_image_key_suffix(path, key)?;
        if dims.layer != 0 {
            continue;
        }
        if dims.col >= num_cols || dims.row >= num_rows {
            return Err(invalid_slide(
                path,
                format!("invalid VMS image coordinates ({},{})", dims.col, dims.row),
            ));
        }
        let idx = (dims.row * num_cols + dims.col) as usize;
        if image_paths[idx].is_some() {
            return Err(invalid_slide(
                path,
                format!("duplicate VMS image for ({},{})", dims.col, dims.row),
            ));
        }
        image_paths[idx] = Some(resolve_companion_file(path, dir, value)?);
    }
    let image_paths: Vec<PathBuf> = image_paths
        .into_iter()
        .enumerate()
        .map(|(idx, path_opt)| {
            path_opt.ok_or_else(|| invalid_slide(path, format!("missing VMS image filename {idx}")))
        })
        .collect::<Result<_, _>>()?;
    if image_paths.iter().collect::<HashSet<_>>().len() != image_paths.len() {
        return Err(invalid_slide(path, "duplicate VMS image file path"));
    }

    let map_path = resolve_companion_file(
        path,
        dir,
        group
            .get(KEY_MAP_FILE)
            .ok_or_else(|| invalid_slide(path, "missing MapFile"))?,
    )?;
    let macro_path = group
        .get(KEY_MACRO_IMAGE)
        .map(|value| resolve_companion_file(path, dir, value))
        .transpose()?;
    let opt_path = group
        .get(KEY_OPTIMISATION_FILE)
        .map(|value| resolve_companion_file(path, dir, value))
        .transpose()?;

    let mut quickhash = Quickhash1::new();
    quickhash.hash_file(path)?;
    quickhash.hash_file(&map_path)?;
    let quickhash = quickhash
        .finish()
        .ok_or_else(|| invalid_slide(path, "failed to compute VMS quickhash"))?;
    let dataset_id = dataset_id_from_quickhash(path, &quickhash, "quickhash")?;
    let opt_offsets = parse_vms_opt_offsets(opt_path.as_deref(), &image_paths)?;

    let mut base_images = Vec::with_capacity(image_paths.len());
    for (idx, image_path) in image_paths.iter().enumerate() {
        let row_starts = opt_offsets.get(idx).cloned().unwrap_or_default();
        base_images.push(Arc::new(VmsJpeg::parse_with_private_cache_budget(
            image_path,
            row_starts,
            &mut private_cache_budget,
            budget.limits().encoded_unit_bytes(),
        )?));
    }
    let map_image = Arc::new(VmsJpeg::parse_with_private_cache_budget(
        &map_path,
        Vec::new(),
        &mut private_cache_budget,
        budget.limits().encoded_unit_bytes(),
    )?);

    Ok(VmsSources {
        group,
        macro_path,
        private_cache_budget,
        quickhash,
        dataset_id,
        base_images,
        map_image,
        num_cols,
        num_rows,
    })
}

fn build_levels_and_properties(
    path: &Path,
    sources: VmsSources,
) -> Result<VmsDatasetParts, WsiError> {
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "hamamatsu");
    properties.insert("openslide.quickhash-1", sources.quickhash);
    for (key, value) in &sources.group {
        properties.insert(format!("hamamatsu.{key}"), value.clone());
    }
    if let Some(first_comment) = sources
        .base_images
        .first()
        .and_then(|image| image.comment.as_deref())
    {
        properties.insert("openslide.comment", first_comment);
    }
    if let Some(source_lens) = sources.group.get(KEY_SOURCE_LENS) {
        properties.insert("openslide.objective-power", source_lens.clone());
    }

    let base_level = VmsLevel::new(sources.base_images, sources.num_cols, sources.num_rows, 1)?;
    let map_level = VmsLevel::new(vec![sources.map_image], 1, 1, 1)?;
    if let Some(width_nm) = sources
        .group
        .get(KEY_PHYSICAL_WIDTH)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
    {
        properties.insert(
            "openslide.mpp-x",
            format!(
                "{}",
                width_nm as f64 / (1000.0 * base_level_dimensions(&base_level).0 as f64)
            ),
        );
    }
    if let Some(height_nm) = sources
        .group
        .get(KEY_PHYSICAL_HEIGHT)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
    {
        properties.insert(
            "openslide.mpp-y",
            format!(
                "{}",
                height_nm as f64 / (1000.0 * base_level_dimensions(&base_level).1 as f64)
            ),
        );
    }

    let levels = expanded_levels(base_level, map_level);
    let largest = base_level_dimensions(
        levels
            .first()
            .ok_or_else(|| invalid_slide(path, "VMS slide created no levels"))?,
    );
    let dataset_levels = levels
        .iter()
        .map(|level| {
            let dimensions = base_level_dimensions(level);
            Level {
                dimensions,
                downsample: largest.0 as f64 / dimensions.0 as f64,
                tile_layout: TileLayout::Regular {
                    tile_width: level.jpegs[0].tile_width / level.scale_denom,
                    tile_height: level.jpegs[0].tile_height / level.scale_denom,
                    tiles_across: total_tiles_across(level),
                    tiles_down: total_tiles_down(level),
                },
            }
        })
        .collect();

    Ok(VmsDatasetParts {
        macro_path: sources.macro_path,
        private_cache_budget: sources.private_cache_budget,
        dataset_id: sources.dataset_id,
        levels,
        dataset_levels,
        properties,
    })
}

fn assemble_slide(mut parts: VmsDatasetParts) -> Result<VmsSlide, WsiError> {
    let encoded_unit_bytes = parts
        .levels
        .first()
        .and_then(|level| level.jpegs.first())
        .map(|jpeg| jpeg.encoded_unit_bytes)
        .ok_or_else(|| invalid_slide(Path::new(""), "VMS slide created no JPEG sources"))?;
    let mut associated_images = HashMap::new();
    let mut associated_paths = HashMap::new();
    if let Some(macro_path) = parts.macro_path.filter(|path| path.is_file()) {
        let macro_bytes = read_file_bounded(&macro_path, encoded_unit_bytes, "VMS macro JPEG")
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: macro_path.clone(),
            })?;
        let macro_dims = jpeg_dimensions(&macro_bytes)?;
        associated_images.insert(
            "macro".into(),
            AssociatedImage {
                dimensions: macro_dims,
                sample_type: SampleType::Uint8,
                channels: 3,
                icc_profile: Vec::new(),
            },
        );
        associated_paths.insert("macro".into(), macro_path);
    }

    let associated_entry_bytes = associated_images
        .values()
        .map(|image| {
            u64::from(image.dimensions.0)
                .saturating_mul(u64::from(image.dimensions.1))
                .saturating_mul(u64::from(image.channels))
        })
        .max()
        .unwrap_or(1);
    let associated_cache =
        PrivateCache::new(parts.private_cache_budget.allocate(associated_entry_bytes));

    let dataset = Dataset {
        id: parts.dataset_id,
        scenes: vec![Scene {
            id: "s0".into(),
            name: None,
            series: vec![Series {
                id: "ser0".into(),
                axes: AxesShape::default(),
                levels: parts.dataset_levels,
                sample_type: SampleType::Uint8,
                channels: vec![],
            }],
        }],
        associated_images,
        properties: parts.properties,
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    };

    Ok(VmsSlide {
        dataset,
        levels: parts.levels,
        associated_paths,
        associated_cache: Mutex::new(associated_cache),
        encoded_unit_bytes,
    })
}
