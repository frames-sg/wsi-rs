use super::attachments::{
    associated_name, decode_associated_attachment, probe_associated_attachment,
};
use super::metadata::*;
use super::preflight::preflight_czi_file;
use super::*;

type LevelImageCache = Mutex<PrivateCache<(usize, usize), Arc<CpuTile>>>;
type LocalTileCache = Mutex<PrivateCache<(usize, usize, i64, i64), Arc<CpuTile>>>;

#[cfg(test)]
pub(super) static ZEISS_LOCAL_TILE_HITS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ZEISS_DIRECT_LEVEL_COMPOSE_HITS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS: AtomicU64 = AtomicU64::new(0);

pub(super) struct ZeissReader {
    pub(super) slide: Arc<ZeissSlide>,
}

impl SlideReader for ZeissReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        read_cpu_tiles_with_backend(
            reqs,
            output,
            "RequireDevice is not supported for Zeiss",
            |req, backend| self.read_tile_with_backend(req, backend),
        )
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.slide.read_associated(name)
    }
}

impl ZeissReader {
    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        self.slide.read_tile(
            req.scene.get(),
            req.series.get(),
            req.level.get(),
            req.col,
            req.row,
            backend,
        )
    }
}

pub(super) struct ZeissSlide {
    pub(super) source_path: PathBuf,
    pub(super) dataset: Dataset,
    pub(super) czi: Mutex<CziFile>,
    pub(super) level_cache: LevelImageCache,
    pub(super) tile_cache: LocalTileCache,
    pub(super) associated_cache: Mutex<PrivateCache<String, Arc<CpuTile>>>,
    pub(super) associated_sources: HashMap<String, czi_rs::AttachmentInfo>,
    pub(super) subblock_origin: (i32, i32),
    pub(super) canvas_level_subblocks: Vec<Vec<usize>>,
    pub(super) canvas_level_tile_subblocks: Vec<CanvasTileSubblockMap>,
}

impl ZeissSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        Self::parse_with_cache_config(path, CacheConfig::deterministic())
    }

    pub(super) fn parse_with_cache_config(
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Self, WsiError> {
        preflight_czi_file(path)?;
        let mut czi = CziFile::open(path)
            .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;

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
                MAX_DECODED_IMAGE_BYTES,
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
        let statistics = czi.statistics().clone();
        let attachments = czi.attachments().to_vec();
        let subblocks = czi.subblocks().to_vec();

        let scene_indices = scene_indices(&statistics, &summary)?;
        if scene_indices.is_empty() {
            return Err(invalid_slide(path, "Zeiss slide has no scenes"));
        }

        let level_ratios = common_level_ratios(&subblocks, &scene_indices, &statistics)?;
        if level_ratios.len() > MAX_CZI_LEVELS {
            return Err(invalid_slide(
                path,
                format!(
                    "CZI level count {} exceeds the {MAX_CZI_LEVELS}-level safety limit",
                    level_ratios.len()
                ),
            ));
        }
        let canvas_origin = canvas_origin(&statistics);
        let subblock_origin = subblock_origin(&subblocks);
        let canvas_dimensions = canvas_dimensions(&statistics, &summary, path)?;
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
        for subblock in &subblocks {
            if !subblock_matches_default_plane(subblock, &statistics) {
                continue;
            }
            let Some(level_ratio) = subblock_ratio(subblock) else {
                continue;
            };
            let Some(level_slot) = level_ratios.iter().position(|ratio| *ratio == level_ratio)
            else {
                continue;
            };
            canvas_level_subblocks[level_slot].push(subblock.index);
        }
        let canvas_level_tile_subblocks = build_canvas_level_tile_subblocks(
            &subblocks,
            &canvas_level_subblocks,
            &levels,
            subblock_origin,
        )?;
        let scenes = vec![Scene {
            id: "scene_0".to_string(),
            name: Some("Canvas".to_string()),
            series: vec![Series {
                id: "series_0".to_string(),
                axes: AxesShape::default(),
                levels,
                sample_type: SampleType::Uint8,
                channels: build_channels(&summary),
            }],
        }];

        let quickhash = quickhash_for_zeiss(&header, &xml)?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash)?;

        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "zeiss");
        properties.insert("openslide.quickhash-1", quickhash.clone());
        if let Some(v) = summary.document.user_name {
            properties.insert("zeiss.document.user_name", v);
        }
        if let Some(v) = summary.document.creation_date {
            properties.insert("zeiss.document.creation_date", v);
        }
        if let Some(v) = summary.document.application_name {
            properties.insert("zeiss.document.application_name", v);
        }
        if let Some(v) = summary.document.application_version {
            properties.insert("zeiss.document.application_version", v);
        }
        if let Some(v) = summary.image.pixel_type {
            properties.insert("zeiss.image.pixel_type", v.as_str());
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
        if let Some(x) = summary.scaling.x {
            let mpp_x = x * 1_000_000.0;
            properties.insert("openslide.mpp-x", format!("{mpp_x:.6}"));
            properties.insert("zeiss.scaling.x", x.to_string());
        }
        if let Some(y) = summary.scaling.y {
            let mpp_y = y * 1_000_000.0;
            properties.insert("openslide.mpp-y", format!("{mpp_y:.6}"));
            properties.insert("zeiss.scaling.y", y.to_string());
        }
        if let Some(objective) = extract_objective_magnification(&xml) {
            properties.insert("openslide.objective-power", objective);
        }

        for (idx, scene_index) in scene_indices.iter().enumerate() {
            if let Some(bounding_boxes) =
                statistics.scene_bounding_boxes.get(&(*scene_index as i32))
            {
                let region = if bounding_boxes.layer0.is_valid() {
                    bounding_boxes.layer0
                } else {
                    bounding_boxes.all
                };
                if region.is_valid() {
                    properties.insert(
                        format!("openslide.region[{idx}].x"),
                        (region.x - canvas_origin.0).to_string(),
                    );
                    properties.insert(
                        format!("openslide.region[{idx}].y"),
                        (region.y - canvas_origin.1).to_string(),
                    );
                    properties.insert(
                        format!("openslide.region[{idx}].width"),
                        region.w.to_string(),
                    );
                    properties.insert(
                        format!("openslide.region[{idx}].height"),
                        region.h.to_string(),
                    );
                }
            }
        }

        let mut associated_images = HashMap::new();
        let mut associated_sources = HashMap::new();
        for attachment in &attachments {
            let Some(name) = associated_name(&attachment.name) else {
                continue;
            };
            if let Some(metadata) = probe_associated_attachment(path, &mut czi, attachment)? {
                associated_images.insert(name.to_string(), metadata);
                associated_sources.insert(name.to_string(), attachment.clone());
            }
        }

        let dataset = Dataset {
            id: dataset_id,
            scenes,
            associated_images,
            properties,
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

        let mut private_cache_budget = cache_config.private_cache_budget(3);
        let level_cache = PrivateCache::new(private_cache_budget.allocate(level_entry_bytes));
        let tile_cache = PrivateCache::new(private_cache_budget.allocate(tile_entry_bytes));
        let associated_cache =
            PrivateCache::new(private_cache_budget.allocate(associated_entry_bytes));

        Ok(Self {
            source_path: path.to_path_buf(),
            dataset,
            czi: Mutex::new(czi),
            level_cache: Mutex::new(level_cache),
            tile_cache: Mutex::new(tile_cache),
            associated_cache: Mutex::new(associated_cache),
            associated_sources,
            subblock_origin,
            canvas_level_subblocks,
            canvas_level_tile_subblocks,
        })
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        if let Some(cached) = self
            .associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(cached.as_ref().clone());
        }

        let attachment = self
            .associated_sources
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        let buffer = {
            let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let (_, buffer) = decode_associated_attachment(&mut czi, attachment)?
                .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
            buffer
        };
        let arc = Arc::new(buffer);
        self.associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(name.to_string(), arc.clone());
        Ok(arc.as_ref().clone())
    }
}
