use super::attachments::{
    associated_name, decode_associated_attachment, probe_associated_attachment,
};
use super::metadata::*;
use super::*;

type LevelImageCache = Mutex<LruCache<(usize, usize), Arc<CpuTile>>>;
type LocalTileCache = Mutex<LruCache<(usize, usize, i64, i64), Arc<CpuTile>>>;

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
        let backend = (match output {
            TileOutputPreference::Cpu { backend }
            | TileOutputPreference::PreferDevice { backend, .. } => backend,
            TileOutputPreference::RequireDevice { .. } => {
                return Err(WsiError::Unsupported {
                    reason: "RequireDevice not supported for Zeiss in Phase 2".into(),
                });
            }
        })
        .to_signinum();
        reqs.iter()
            .map(|req| {
                self.read_tile_with_backend(req, backend)
                    .map(TilePixels::Cpu)
            })
            .collect()
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
    pub(super) dataset: Dataset,
    pub(super) czi: Mutex<CziFile>,
    pub(super) level_cache: LevelImageCache,
    pub(super) tile_cache: LocalTileCache,
    pub(super) associated_cache: Mutex<LruCache<String, Arc<CpuTile>>>,
    pub(super) associated_sources: HashMap<String, czi_rs::AttachmentInfo>,
    pub(super) subblock_origin: (i32, i32),
    pub(super) canvas_level_subblocks: Vec<Vec<usize>>,
    pub(super) canvas_level_tile_subblocks: Vec<StdHashMap<(i64, i64), Vec<usize>>>,
}

impl ZeissSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        let mut czi = CziFile::open(path)
            .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;

        let header = czi.file_header().clone();
        let xml = czi
            .metadata_xml()
            .map_err(|source| WsiError::DisplayConversion(source.to_string()))?
            .to_string();
        let summary = czi
            .metadata()
            .map_err(|source| WsiError::DisplayConversion(source.to_string()))?
            .clone();
        let statistics = czi.statistics().clone();
        let attachments = czi.attachments().to_vec();
        let subblocks = czi.subblocks().to_vec();

        let scene_indices = scene_indices(&statistics, &summary);
        if scene_indices.is_empty() {
            return Err(invalid_slide(path, "Zeiss slide has no scenes"));
        }

        let level_ratios = common_level_ratios(&subblocks, &scene_indices, &statistics);
        let canvas_origin = canvas_origin(&statistics);
        let subblock_origin = subblock_origin(&subblocks);
        let canvas_dimensions = canvas_dimensions(&statistics, &summary, path)?;
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
        );
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

        Ok(Self {
            dataset,
            czi: Mutex::new(czi),
            level_cache: Mutex::new(LruCache::new(std::num::NonZeroUsize::new(1).unwrap())),
            tile_cache: Mutex::new(LruCache::new(std::num::NonZeroUsize::new(8).unwrap())),
            associated_cache: Mutex::new(LruCache::new(std::num::NonZeroUsize::new(4).unwrap())),
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
