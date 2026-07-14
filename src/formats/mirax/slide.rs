use super::helpers::*;
use super::index::*;
use super::*;

const MAX_MIRAX_BASE_IMAGES: u64 = 16 * 1024 * 1024;
const MAX_MIRAX_HIERARCHIES: i32 = 1_024;
const MAX_MIRAX_NONHIERARCHIES: i32 = 4_096;
const MAX_MIRAX_ZOOM_LEVELS: i32 = 64;
const MAX_MIRAX_DATA_FILES: i32 = 4_096;

impl MiraxSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        let slide_dir = slide_dir_from_entry(path)?;
        let slidedat_path = slide_dir.join(SLIDEDAT_INI);
        let slidedat = parse_mirax_ini(&slidedat_path)?;

        let general = slidedat
            .groups
            .get(GROUP_GENERAL)
            .ok_or_else(|| invalid_slide(path, "missing [GENERAL] group"))?;
        let hierarchical = slidedat
            .groups
            .get(GROUP_HIERARCHICAL)
            .ok_or_else(|| invalid_slide(path, "missing [HIERARCHICAL] group"))?;
        let datafile_group = slidedat
            .groups
            .get(GROUP_DATAFILE)
            .ok_or_else(|| invalid_slide(path, "missing [DATAFILE] group"))?;

        let slide_id = required_ini_string(path, general, KEY_SLIDE_ID)?;
        let images_x = parse_ini_u32(path, general, KEY_IMAGE_NUMBER_X)?;
        let images_y = parse_ini_u32(path, general, KEY_IMAGE_NUMBER_Y)?;
        let objective_magnification = parse_ini_i32(path, general, KEY_OBJECTIVE_MAGNIFICATION)?;
        let image_divisions = general
            .get(KEY_CAMERA_IMAGE_DIVISIONS_PER_SIDE)
            .map(|value| parse_u32_value(path, KEY_CAMERA_IMAGE_DIVISIONS_PER_SIDE, value))
            .transpose()?
            .unwrap_or(1);
        if images_x == 0 || images_y == 0 || image_divisions == 0 {
            return Err(invalid_slide(path, "MIRAX image counts must be positive"));
        }
        if images_x < image_divisions
            || images_y < image_divisions
            || u64::from(images_x)
                .checked_mul(u64::from(images_y))
                .is_none_or(|count| count > MAX_MIRAX_BASE_IMAGES)
        {
            return Err(invalid_slide(
                path,
                "MIRAX image grid exceeds supported safety limits",
            ));
        }

        let hier_count = parse_ini_i32(path, hierarchical, KEY_HIER_COUNT)?;
        let nonhier_count = parse_ini_i32(path, hierarchical, KEY_NONHIER_COUNT)?;
        if hier_count <= 0
            || hier_count > MAX_MIRAX_HIERARCHIES
            || !(0..=MAX_MIRAX_NONHIERARCHIES).contains(&nonhier_count)
        {
            return Err(invalid_slide(
                path,
                "MIRAX hierarchy counts must be positive/non-negative",
            ));
        }

        let slide_zoom_level_value = (0..hier_count)
            .find(|idx| {
                hierarchical
                    .get(&fmt_key(KEY_HIER_NAME, *idx))
                    .map(|value| value == VALUE_SLIDE_ZOOM_LEVEL)
                    .unwrap_or(false)
            })
            .ok_or_else(|| invalid_slide(path, "cannot find Slide zoom level hierarchy"))?;
        if slide_zoom_level_value != 0 {
            return Err(invalid_slide(path, "Slide zoom level not HIER_0"));
        }

        let index_filename = required_ini_string(path, hierarchical, KEY_INDEXFILE)?;
        let index_path = resolve_companion_file(path, &slide_dir, &index_filename)?;
        let zoom_levels = parse_ini_i32(path, hierarchical, &fmt_key(KEY_HIER_COUNT_FMT, 0))?;
        if zoom_levels <= 0 || zoom_levels > MAX_MIRAX_ZOOM_LEVELS {
            return Err(invalid_slide(path, "MIRAX slide has no zoom levels"));
        }
        let zoom_sections = (0..zoom_levels)
            .map(|idx| {
                required_ini_string(
                    path,
                    hierarchical,
                    &fmt_key2(KEY_HIER_VAL_SECTION_FMT, 0, idx),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let datafile_count = parse_ini_i32(path, datafile_group, KEY_FILE_COUNT)?;
        if datafile_count <= 0 || datafile_count > MAX_MIRAX_DATA_FILES {
            return Err(invalid_slide(path, "MIRAX slide has no data files"));
        }
        let datafile_paths = (0..datafile_count)
            .map(|idx| {
                required_ini_string(path, datafile_group, &fmt_key(KEY_FILE_FMT, idx))
                    .and_then(|name| resolve_companion_file(path, &slide_dir, &name))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if datafile_paths.iter().collect::<HashSet<_>>().len() != datafile_paths.len() {
            return Err(invalid_slide(path, "duplicate MIRAX data file path"));
        }

        let mut section_defs = Vec::with_capacity(zoom_levels as usize);
        for (idx, section_name) in zoom_sections.iter().enumerate() {
            let group = slidedat.groups.get(section_name).ok_or_else(|| {
                invalid_slide(path, format!("missing MIRAX section {section_name}"))
            })?;
            let concat_exponent = parse_ini_i32(path, group, KEY_IMAGE_CONCAT_FACTOR)?;
            if (idx == 0 && concat_exponent < 0) || (idx > 0 && concat_exponent <= 0) {
                return Err(invalid_slide(
                    path,
                    format!("invalid IMAGE_CONCAT_FACTOR on {section_name}"),
                ));
            }
            let image_w = parse_ini_u32(path, group, KEY_DIGITIZER_WIDTH)?;
            let image_h = parse_ini_u32(path, group, KEY_DIGITIZER_HEIGHT)?;
            if image_w == 0 || image_h == 0 {
                return Err(invalid_slide(
                    path,
                    format!("zero digitizer size on {section_name}"),
                ));
            }
            let bgr = parse_ini_u32(path, group, KEY_IMAGE_FILL_COLOR_BGR)?;
            section_defs.push(SlideZoomLevelSection {
                concat_exponent,
                overlap_x: parse_ini_f64(path, group, KEY_OVERLAP_X)?,
                overlap_y: parse_ini_f64(path, group, KEY_OVERLAP_Y)?,
                mpp_x: parse_ini_f64(path, group, KEY_MPP_X)?,
                mpp_y: parse_ini_f64(path, group, KEY_MPP_Y)?,
                fill_rgb: bgr_to_rgb(bgr),
                image_format: parse_image_format(
                    required_ini_string(path, group, KEY_IMAGE_FORMAT)?.as_str(),
                )?,
                image_w,
                image_h,
            });
        }

        let position_nonhier_vimslide_offset = get_nonhier_name_offset(
            path,
            &slidedat,
            nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_VIMSLIDE_POSITION_BUFFER,
        )?;
        let position_nonhier_stitching_offset = if position_nonhier_vimslide_offset.is_some() {
            None
        } else {
            get_nonhier_name_offset(
                path,
                &slidedat,
                nonhier_count,
                GROUP_HIERARCHICAL,
                VALUE_STITCHING_INTENSITY_LAYER,
            )?
        };

        let macro_nonhier_offset = get_associated_image_nonhier_offset(
            path,
            &slidedat,
            nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_MACRO,
            KEY_MACRO_IMAGE_TYPE,
        )?;
        let label_nonhier_offset = get_associated_image_nonhier_offset(
            path,
            &slidedat,
            nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_LABEL,
            KEY_LABEL_IMAGE_TYPE,
        )?;
        let thumbnail_nonhier_offset = get_associated_image_nonhier_offset(
            path,
            &slidedat,
            nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_THUMBNAIL,
            KEY_THUMBNAIL_IMAGE_TYPE,
        )?;

        let mut quickhash = Quickhash1::new();
        quickhash.hash_file(&slidedat_path)?;

        let mut base_w = 0i64;
        let mut base_h = 0i64;
        for i in 0..images_x {
            if (i % image_divisions) != image_divisions - 1 || i == images_x - 1 {
                base_w = base_w
                    .checked_add(i64::from(section_defs[0].image_w))
                    .ok_or_else(|| invalid_slide(path, "MIRAX base width overflow"))?;
            } else {
                base_w = base_w
                    .checked_add(
                        (f64::from(section_defs[0].image_w) - section_defs[0].overlap_x) as i64,
                    )
                    .ok_or_else(|| invalid_slide(path, "MIRAX base width overflow"))?;
            }
        }
        for i in 0..images_y {
            if (i % image_divisions) != image_divisions - 1 || i == images_y - 1 {
                base_h = base_h
                    .checked_add(i64::from(section_defs[0].image_h))
                    .ok_or_else(|| invalid_slide(path, "MIRAX base height overflow"))?;
            } else {
                base_h = base_h
                    .checked_add(
                        (f64::from(section_defs[0].image_h) - section_defs[0].overlap_y) as i64,
                    )
                    .ok_or_else(|| invalid_slide(path, "MIRAX base height overflow"))?;
            }
        }
        if base_w <= 0 || base_h <= 0 {
            return Err(invalid_slide(path, "invalid MIRAX base dimensions"));
        }

        let mut params = Vec::with_capacity(section_defs.len());
        let mut level_builders = Vec::with_capacity(section_defs.len());
        let mut total_concat_exponent = 0i32;
        for (idx, section) in section_defs.iter().enumerate() {
            total_concat_exponent = total_concat_exponent
                .checked_add(section.concat_exponent)
                .ok_or_else(|| invalid_slide(path, "MIRAX concat exponent overflow"))?;
            if total_concat_exponent >= 30 {
                return Err(invalid_slide(path, "MIRAX concat exponent too large"));
            }
            let image_concat = 1u32 << total_concat_exponent;
            let positions_per_image = (image_concat / image_divisions).max(1);
            let (tile_count_divisor, tiles_per_image, positions_per_tile) =
                if position_nonhier_vimslide_offset.is_some()
                    || position_nonhier_stitching_offset.is_some()
                    || section_defs[0].overlap_x != 0.0
                    || section_defs[0].overlap_y != 0.0
                {
                    (image_concat.min(image_divisions), positions_per_image, 1)
                } else {
                    (image_concat, 1, positions_per_image)
                };
            let tile_w = f64::from(section.image_w) / f64::from(tiles_per_image);
            let tile_h = f64::from(section.image_h) / f64::from(tiles_per_image);
            let images_per_position = (image_divisions / image_concat).max(1);
            let tile_advance_x = tile_w - section.overlap_x / f64::from(images_per_position);
            let tile_advance_y = tile_h - section.overlap_y / f64::from(images_per_position);
            let level_dimensions = (
                (base_w / i64::from(image_concat)) as u64,
                (base_h / i64::from(image_concat)) as u64,
            );
            let downsample =
                f64::from(image_concat) / f64::from(1u32 << section_defs[0].concat_exponent.max(0));
            params.push(SlideZoomLevelParams {
                image_concat,
                tile_count_divisor,
                tiles_per_image,
                positions_per_tile,
                tile_advance_x,
                tile_advance_y,
            });
            level_builders.push(MiraxLevelBuilder {
                dimensions: level_dimensions,
                downsample,
                image_format: section.image_format,
                raw_image_width: section.image_w,
                raw_image_height: section.image_h,
                tile_width: tile_w,
                tile_height: tile_h,
                tile_advance_x,
                tile_advance_y,
                tiles: HashMap::new(),
                descriptors: Vec::new(),
                extra_tiles: (0, 0, 0, 0),
            });
            if !tile_advance_x.is_finite()
                || !tile_advance_y.is_finite()
                || tile_advance_x <= 0.0
                || tile_advance_y <= 0.0
            {
                return Err(invalid_slide(
                    path,
                    format!("invalid MIRAX tile advance at level {idx}"),
                ));
            }
        }

        let mut index_file = File::open(&index_path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: index_path.clone(),
        })?;
        verify_index_header(path, &mut index_file, &slide_id)?;

        let associated = build_associated_records(
            path,
            &mut index_file,
            &datafile_paths,
            slide_id.len(),
            macro_nonhier_offset,
            label_nonhier_offset,
            thumbnail_nonhier_offset,
        )?;

        let slide_positions = load_slide_positions(
            path,
            &mut index_file,
            &datafile_paths,
            slide_id.len(),
            position_nonhier_vimslide_offset,
            position_nonhier_stitching_offset,
            images_x,
            images_y,
            image_divisions,
            params[0].image_concat,
            section_defs[0].image_w,
            section_defs[0].image_h,
            section_defs[0].overlap_x,
            section_defs[0].overlap_y,
        )?;

        let hier_root = (INDEX_VERSION.len() + slide_id.len()) as u64;
        index_file
            .seek(SeekFrom::Start(hier_root))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: index_path.clone(),
            })?;
        let seek_location = read_u32_le(&mut index_file, &index_path)? as u64;

        let mut quickhash_files = HashMap::new();
        process_hier_data_pages_from_indexfile(MiraxIndexBuildContext {
            path,
            index_file: &mut index_file,
            index_path: &index_path,
            seek_location,
            datafile_paths: &datafile_paths,
            images: (images_x, images_y),
            image_divisions,
            params: &params,
            levels: &mut level_builders,
            slide_positions: &slide_positions,
            quickhash: &mut quickhash,
            quickhash_files: &mut quickhash_files,
        })?;

        let quickhash = quickhash
            .finish()
            .ok_or_else(|| invalid_slide(path, "failed to compute MIRAX quickhash"))?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash)?;

        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "mirax");
        properties.insert("openslide.quickhash-1", quickhash.clone());
        properties.insert(
            "openslide.objective-power",
            objective_magnification.to_string(),
        );
        properties.insert("openslide.mpp-x", format!("{}", section_defs[0].mpp_x));
        properties.insert("openslide.mpp-y", format!("{}", section_defs[0].mpp_y));
        properties.insert(
            "openslide.background-color",
            format!("{:06X}", section_defs[0].fill_rgb),
        );

        let mut associated_metadata = HashMap::new();
        for (name, record) in &associated {
            let dimensions = read_jpeg_dimensions_from_record(path, &mut quickhash_files, record)
                .map_err(|err| {
                invalid_slide(
                    path,
                    format!("failed to read MIRAX associated image {name} dimensions: {err}"),
                )
            })?;
            associated_metadata.insert(
                name.clone(),
                AssociatedImage {
                    dimensions,
                    sample_type: SampleType::Uint8,
                    channels: 3,
                },
            );
        }

        let mut dataset_levels = Vec::with_capacity(level_builders.len());
        let mut levels = Vec::with_capacity(level_builders.len());
        for level in level_builders {
            dataset_levels.push(Level {
                dimensions: level.dimensions,
                downsample: level.downsample,
                tile_layout: TileLayout::Irregular {
                    tile_advance: (level.tile_advance_x, level.tile_advance_y),
                    extra_tiles: level.extra_tiles,
                    tiles: level.tiles,
                },
            });
            levels.push(MiraxLevel {
                tiles: level.descriptors,
            });
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
            associated_images: associated_metadata,
            properties,
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        };

        Ok(Self {
            dataset,
            levels,
            associated,
            decoded_images: Mutex::new(LruCache::new(NonZeroUsize::new(1).unwrap())),
            associated_cache: Mutex::new(LruCache::new(NonZeroUsize::new(1).unwrap())),
            open_files: Mutex::new(quickhash_files),
        })
    }

    pub(super) fn decode_image_with_backend(
        &self,
        image: &Arc<MiraxImage>,
        _backend: BackendRequest,
    ) -> Result<Arc<CpuTile>, WsiError> {
        let mut cache = self
            .decoded_images
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(buffer) = cache.get(&image.id) {
            return Ok(buffer.clone());
        }
        let decoded = Arc::new(self.decode_record_to_sample_buffer(
            &image.record,
            image.format,
            Some((image.expected_width, image.expected_height)),
            BackendRequest::Auto,
        )?);
        cache.put(image.id, decoded.clone());
        Ok(decoded)
    }

    pub(super) fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let record = self
            .associated
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        let mut cache = self
            .associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(buffer) = cache.get(name) {
            #[cfg(test)]
            {
                MIRAX_ASSOCIATED_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            }
            return Ok((**buffer).clone());
        }
        let decoded = Arc::new(self.decode_record_to_sample_buffer(
            record,
            MiraxImageFormat::Jpeg,
            None,
            BackendRequest::Auto,
        )?);
        cache.put(name.to_string(), decoded.clone());
        Ok((*decoded).clone())
    }

    fn decode_record_to_sample_buffer(
        &self,
        record: &MiraxRecord,
        format: MiraxImageFormat,
        expected_dimensions: Option<(u32, u32)>,
        _backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let bytes = self.read_record_bytes(record)?;
        match format {
            MiraxImageFormat::Jpeg => {
                let (expected_width, expected_height) = expected_dimensions.unwrap_or((0, 0));
                crate::core::batch::exactly_one(
                    decode_batch_jpeg(&[JpegDecodeJob {
                        data: Cow::Borrowed(&bytes),
                        tables: None,
                        expected_width,
                        expected_height,
                        color_transform: j2k_jpeg::ColorTransform::Auto,
                        force_dimensions: false,
                        requested_size: None,
                    }]),
                    "MIRAX JPEG decode",
                )?
            }
            MiraxImageFormat::Png | MiraxImageFormat::Bmp24 => {
                let image = image::load_from_memory(&bytes)
                    .map_err(|err| {
                        WsiError::DisplayConversion(format!("failed to decode MIRAX image: {err}"))
                    })?
                    .to_rgb8();
                if let Some((expected_width, expected_height)) = expected_dimensions {
                    if image.width() != expected_width || image.height() != expected_height {
                        return Err(WsiError::DisplayConversion(format!(
                            "MIRAX image dimensions mismatch: expected {}x{}, got {}x{}",
                            expected_width,
                            expected_height,
                            image.width(),
                            image.height()
                        )));
                    }
                }
                Ok(rgb_image_to_sample_buffer(image))
            }
        }
    }

    fn read_record_bytes(&self, record: &MiraxRecord) -> Result<Vec<u8>, WsiError> {
        self.with_open_file(&record.path, |file| {
            read_record_bytes_from_file(file, &record.path, record.offset, record.len)
        })
    }

    fn with_open_file<T>(
        &self,
        path: &Path,
        f: impl FnOnce(&mut File) -> Result<T, WsiError>,
    ) -> Result<T, WsiError> {
        let mut files = self.open_files.lock().unwrap_or_else(|e| e.into_inner());
        if !files.contains_key(path) {
            let file = File::open(path).map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
            files.insert(path.to_path_buf(), file);
        }
        let file = files
            .get_mut(path)
            .expect("MIRAX cached file must exist after insertion");
        f(file)
    }
}
