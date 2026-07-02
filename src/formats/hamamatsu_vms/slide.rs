use super::ini::*;
use super::levels::{base_level_dimensions, expanded_levels, total_tiles_across, total_tiles_down};
use super::model::{dataset_id_from_quickhash, invalid_slide, VmsJpeg, VmsLevel, VmsSlide};
use super::*;

const VMS_ASSOCIATED_CACHE_ENTRIES: usize = 4;

pub(super) struct VmsReader {
    pub(super) slide: Arc<VmsSlide>,
}

impl SlideReader for VmsReader {
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
            "RequireDevice not supported for VMS in Phase 2",
            |req, backend| self.read_tile_with_backend(req, backend),
        )
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        if let Some(cached) = self
            .slide
            .associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(cached.as_ref().clone());
        }
        let path = self
            .slide
            .associated_paths
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        let data = std::fs::read(path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.clone(),
        })?;
        decode_batch_jpeg(&[JpegDecodeJob {
            data: Cow::Borrowed(&data),
            tables: None,
            expected_width: 0,
            expected_height: 0,
            color_transform: j2k_jpeg::ColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        }])
        .into_iter()
        .next()
        .expect("1-element JPEG facade batch")
        .map(|tile| {
            let tile = Arc::new(tile);
            self.slide
                .associated_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .put(name.to_string(), tile.clone());
            tile.as_ref().clone()
        })
    }
}

impl VmsReader {
    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let series = &self.slide.dataset.scenes[req.scene.get()].series[req.series.get()];
        let level_meta = &series.levels[req.level.get() as usize];
        let level =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;

        let TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } = level_meta.tile_layout
        else {
            return Err(WsiError::UnsupportedFormat(
                "VMS levels must use regular tiles".into(),
            ));
        };

        if req.col < 0
            || req.row < 0
            || req.col >= tiles_across as i64
            || req.row >= tiles_down as i64
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "tile ({},{}) out of range ({}x{})",
                    req.col, req.row, tiles_across, tiles_down
                ),
            });
        }

        let base_tiles_across = level.base_tiles_across;
        let base_tiles_down = level.base_tiles_down;
        let jpeg_col = req.col as u32 / base_tiles_across;
        let jpeg_row = req.row as u32 / base_tiles_down;
        let local_tile_col = req.col as u32 % base_tiles_across;
        let local_tile_row = req.row as u32 % base_tiles_down;
        let jpeg = level
            .jpegs
            .get((jpeg_row * level.jpegs_across + jpeg_col) as usize)
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "VMS tile resolved to missing JPEG shard".into(),
            })?;
        if local_tile_col >= jpeg.tiles_across || local_tile_row >= jpeg.tiles_down {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "VMS local tile coordinates out of JPEG shard bounds".into(),
            });
        }
        let tile_index = (local_tile_row * jpeg.tiles_across + local_tile_col) as usize;
        jpeg.decode_tile(tile_index, level.scale_denom, backend)
            .map_err(|err| match err {
                WsiError::TileRead { .. } => err,
                other => WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: other.to_string(),
                },
            })
    }
}

impl VmsSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        let ini = parse_vms_ini(path)?;
        let group = ini
            .groups
            .get(GROUP_VMS)
            .ok_or_else(|| invalid_slide(path, "missing [Virtual Microscope Specimen] group"))?;
        let num_cols = parse_u32(path, group, KEY_NUM_JPEG_COLS)?;
        let num_rows = parse_u32(path, group, KEY_NUM_JPEG_ROWS)?;
        if num_cols == 0 || num_rows == 0 {
            return Err(invalid_slide(path, "VMS file has no columns or rows"));
        }

        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut image_paths = vec![None; (num_cols * num_rows) as usize];
        for (key, value) in group {
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
            image_paths[idx] = Some(dir.join(value));
        }
        let image_paths: Vec<PathBuf> = image_paths
            .into_iter()
            .enumerate()
            .map(|(idx, path_opt)| {
                path_opt
                    .ok_or_else(|| invalid_slide(path, format!("missing VMS image filename {idx}")))
            })
            .collect::<Result<_, _>>()?;

        let map_path = dir.join(
            group
                .get(KEY_MAP_FILE)
                .ok_or_else(|| invalid_slide(path, "missing MapFile"))?,
        );
        let macro_path = group.get(KEY_MACRO_IMAGE).map(|value| dir.join(value));
        let opt_path = group
            .get(KEY_OPTIMISATION_FILE)
            .map(|value| dir.join(value));

        let mut quickhash = Quickhash1::new();
        quickhash.hash_file(path)?;
        quickhash.hash_file(&map_path)?;
        let quickhash = quickhash
            .finish()
            .ok_or_else(|| invalid_slide(path, "failed to compute VMS quickhash"))?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash)?;

        let opt_offsets = parse_vms_opt_offsets(opt_path.as_deref(), &image_paths)?;

        let mut base_images = Vec::with_capacity(image_paths.len());
        for (idx, image_path) in image_paths.iter().enumerate() {
            let row_starts = opt_offsets.get(idx).cloned().unwrap_or_default();
            base_images.push(Arc::new(VmsJpeg::parse(image_path, row_starts)?));
        }
        let map_image = Arc::new(VmsJpeg::parse(&map_path, Vec::new())?);

        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "hamamatsu");
        properties.insert("openslide.quickhash-1", quickhash.clone());
        for (key, value) in group {
            properties.insert(format!("hamamatsu.{key}"), value.clone());
        }
        if let Some(first_comment) = base_images
            .first()
            .and_then(|image| image.comment.as_deref())
        {
            properties.insert("openslide.comment", first_comment);
        }
        if let Some(source_lens) = group.get(KEY_SOURCE_LENS) {
            properties.insert("openslide.objective-power", source_lens.clone());
        }

        let base_level = VmsLevel::new(base_images, num_cols, num_rows, 1)?;
        let map_level = VmsLevel::new(vec![map_image], 1, 1, 1)?;
        if let Some(width_nm) = group
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
        if let Some(height_nm) = group
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
        let dataset_levels: Vec<Level> = levels
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

        let mut associated_images = HashMap::new();
        let mut associated_paths = HashMap::new();
        if let Some(macro_path) = macro_path.filter(|p| p.is_file()) {
            let macro_bytes =
                std::fs::read(&macro_path).map_err(|source| WsiError::IoWithPath {
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
                },
            );
            associated_paths.insert("macro".into(), macro_path);
        }

        let dataset = Dataset {
            id: dataset_id,
            scenes: vec![Scene {
                id: "s0".into(),
                name: None,
                series: vec![Series {
                    id: "ser0".into(),
                    axes: AxesShape::default(),
                    levels: dataset_levels,
                    sample_type: SampleType::Uint8,
                    channels: vec![],
                }],
            }],
            associated_images,
            properties,
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        };

        Ok(Self {
            dataset,
            levels,
            associated_paths,
            associated_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(VMS_ASSOCIATED_CACHE_ENTRIES).unwrap(),
            )),
        })
    }
}
