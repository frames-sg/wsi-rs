use super::super::helpers::*;
use super::super::index::*;
use super::super::*;

const MAX_MIRAX_BASE_IMAGES: u64 = 16 * 1024 * 1024;
const MAX_MIRAX_HIERARCHIES: i32 = 1_024;
const MAX_MIRAX_NONHIERARCHIES: i32 = 4_096;
const MAX_MIRAX_ZOOM_LEVELS: i32 = 64;
const MAX_MIRAX_DATA_FILES: i32 = 4_096;

struct MiraxIniPreflight {
    slidedat_path: PathBuf,
    slidedat: ParsedIni,
    slide_id: String,
    images: (u32, u32),
    objective_magnification: i32,
    image_divisions: u32,
    index_path: PathBuf,
    zoom_sections: Vec<String>,
    nonhier_count: i32,
    datafile_paths: Vec<PathBuf>,
}

struct MiraxHierarchyGeometry {
    ini: MiraxIniPreflight,
    level0_section: SlideZoomLevelSection,
    params: Vec<SlideZoomLevelParams>,
    level_builders: Vec<MiraxLevelBuilder>,
    position_offsets: (Option<i32>, Option<i32>),
    associated_offsets: [Option<i32>; 3],
}

struct MiraxIndexSources {
    hierarchy: MiraxHierarchyGeometry,
    associated: HashMap<String, MiraxRecord>,
    quickhash_files: HashMap<PathBuf, File>,
    quickhash: String,
    dataset_id: DatasetId,
}

impl MiraxSlide {
    pub(in crate::formats::mirax) fn parse_with_cache_config(
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Self, WsiError> {
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

        let ini = MiraxIniPreflight {
            slidedat_path,
            slidedat,
            slide_id,
            images: (images_x, images_y),
            objective_magnification,
            image_divisions,
            index_path,
            zoom_sections,
            nonhier_count,
            datafile_paths,
        };
        let (hierarchy, quickhash) = build_hierarchy_geometry(path, ini)?;
        let mut sources = load_data_index_sources(path, hierarchy, quickhash)?;
        let (properties, associated_metadata) =
            build_associated_images_and_properties(path, &mut sources)?;
        Ok(assemble_dataset_and_caches(
            cache_config,
            sources,
            properties,
            associated_metadata,
        ))
    }
}

fn build_hierarchy_geometry(
    path: &Path,
    ini: MiraxIniPreflight,
) -> Result<(MiraxHierarchyGeometry, Quickhash1), WsiError> {
    let mut section_defs = Vec::with_capacity(ini.zoom_sections.len());
    for (idx, section_name) in ini.zoom_sections.iter().enumerate() {
        let group =
            ini.slidedat.groups.get(section_name).ok_or_else(|| {
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
        &ini.slidedat,
        ini.nonhier_count,
        GROUP_HIERARCHICAL,
        VALUE_VIMSLIDE_POSITION_BUFFER,
    )?;
    let position_nonhier_stitching_offset = if position_nonhier_vimslide_offset.is_some() {
        None
    } else {
        get_nonhier_name_offset(
            path,
            &ini.slidedat,
            ini.nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_STITCHING_INTENSITY_LAYER,
        )?
    };
    let position_offsets = (
        position_nonhier_vimslide_offset,
        position_nonhier_stitching_offset,
    );

    let associated_offsets = [
        get_associated_image_nonhier_offset(
            path,
            &ini.slidedat,
            ini.nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_MACRO,
            KEY_MACRO_IMAGE_TYPE,
        )?,
        get_associated_image_nonhier_offset(
            path,
            &ini.slidedat,
            ini.nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_LABEL,
            KEY_LABEL_IMAGE_TYPE,
        )?,
        get_associated_image_nonhier_offset(
            path,
            &ini.slidedat,
            ini.nonhier_count,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_THUMBNAIL,
            KEY_THUMBNAIL_IMAGE_TYPE,
        )?,
    ];

    let mut quickhash = Quickhash1::new();
    quickhash.hash_file(&ini.slidedat_path)?;
    let (images_x, images_y) = ini.images;
    let image_divisions = ini.image_divisions;
    let base_w = base_dimension(
        images_x,
        image_divisions,
        section_defs[0].image_w,
        section_defs[0].overlap_x,
    )
    .ok_or_else(|| invalid_slide(path, "MIRAX base width overflow"))?;
    let base_h = base_dimension(
        images_y,
        image_divisions,
        section_defs[0].image_h,
        section_defs[0].overlap_y,
    )
    .ok_or_else(|| invalid_slide(path, "MIRAX base height overflow"))?;
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
            if position_offsets.0.is_some()
                || position_offsets.1.is_some()
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
    Ok((
        MiraxHierarchyGeometry {
            ini,
            level0_section: section_defs[0],
            params,
            level_builders,
            position_offsets,
            associated_offsets,
        },
        quickhash,
    ))
}

fn base_dimension(count: u32, divisions: u32, image_size: u32, overlap: f64) -> Option<i64> {
    (0..count).try_fold(0i64, |size, index| {
        let advance = if (index % divisions) != divisions - 1 || index == count - 1 {
            i64::from(image_size)
        } else {
            (f64::from(image_size) - overlap) as i64
        };
        size.checked_add(advance)
    })
}

fn load_data_index_sources(
    path: &Path,
    mut hierarchy: MiraxHierarchyGeometry,
    mut quickhash: Quickhash1,
) -> Result<MiraxIndexSources, WsiError> {
    let ini = &hierarchy.ini;
    let mut index_file = File::open(&ini.index_path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: ini.index_path.clone(),
    })?;
    verify_index_header(path, &mut index_file, &ini.slide_id)?;

    let associated = build_associated_records(
        path,
        &mut index_file,
        &ini.datafile_paths,
        ini.slide_id.len(),
        hierarchy.associated_offsets[0],
        hierarchy.associated_offsets[1],
        hierarchy.associated_offsets[2],
    )?;

    let slide_positions = load_slide_positions(
        path,
        &mut index_file,
        &ini.datafile_paths,
        ini.slide_id.len(),
        hierarchy.position_offsets.0,
        hierarchy.position_offsets.1,
        ini.images.0,
        ini.images.1,
        ini.image_divisions,
        hierarchy.params[0].image_concat,
        hierarchy.level0_section.image_w,
        hierarchy.level0_section.image_h,
        hierarchy.level0_section.overlap_x,
        hierarchy.level0_section.overlap_y,
    )?;

    let hier_root = (INDEX_VERSION.len() + ini.slide_id.len()) as u64;
    index_file
        .seek(SeekFrom::Start(hier_root))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: ini.index_path.clone(),
        })?;
    let seek_location = read_u32_le(&mut index_file, &ini.index_path)? as u64;

    let mut quickhash_files = HashMap::new();
    process_hier_data_pages_from_indexfile(MiraxIndexBuildContext {
        path,
        index_file: &mut index_file,
        index_path: &ini.index_path,
        seek_location,
        datafile_paths: &ini.datafile_paths,
        images: ini.images,
        image_divisions: ini.image_divisions,
        params: &hierarchy.params,
        levels: &mut hierarchy.level_builders,
        slide_positions: &slide_positions,
        quickhash: &mut quickhash,
        quickhash_files: &mut quickhash_files,
    })?;

    let quickhash = quickhash
        .finish()
        .ok_or_else(|| invalid_slide(path, "failed to compute MIRAX quickhash"))?;
    let dataset_id = dataset_id_from_quickhash(path, &quickhash, "quickhash")?;
    Ok(MiraxIndexSources {
        hierarchy,
        associated,
        quickhash_files,
        quickhash,
        dataset_id,
    })
}

fn build_associated_images_and_properties(
    path: &Path,
    sources: &mut MiraxIndexSources,
) -> Result<(Properties, HashMap<String, AssociatedImage>), WsiError> {
    let level0 = sources.hierarchy.level0_section;
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "mirax");
    properties.insert("openslide.quickhash-1", sources.quickhash.clone());
    properties.insert(
        "openslide.objective-power",
        sources.hierarchy.ini.objective_magnification.to_string(),
    );
    properties.insert("openslide.mpp-x", format!("{}", level0.mpp_x));
    properties.insert("openslide.mpp-y", format!("{}", level0.mpp_y));
    properties.insert(
        "openslide.background-color",
        format!("{:06X}", level0.fill_rgb),
    );
    if let Some((x, y, width, height)) =
        occupied_level_bounds(path, &sources.hierarchy.level_builders[0])?
    {
        properties.insert("openslide.bounds-x", x.to_string());
        properties.insert("openslide.bounds-y", y.to_string());
        properties.insert("openslide.bounds-width", width.to_string());
        properties.insert("openslide.bounds-height", height.to_string());
    }

    let mut associated_metadata = HashMap::new();
    for (name, record) in &sources.associated {
        let dimensions =
            read_jpeg_dimensions_from_record(path, &mut sources.quickhash_files, record).map_err(
                |err| {
                    invalid_slide(
                        path,
                        format!("failed to read MIRAX associated image {name} dimensions: {err}"),
                    )
                },
            )?;
        associated_metadata.insert(
            name.clone(),
            AssociatedImage {
                dimensions,
                sample_type: SampleType::Uint8,
                channels: 3,
            },
        );
    }
    Ok((properties, associated_metadata))
}

fn occupied_level_bounds(
    path: &Path,
    level: &MiraxLevelBuilder,
) -> Result<Option<(i64, i64, u64, u64)>, WsiError> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for (&(tile_x, tile_y), tile) in &level.tiles {
        let left = tile_x as f64 * level.tile_advance_x + tile.offset.0;
        let top = tile_y as f64 * level.tile_advance_y + tile.offset.1;
        let right = left + f64::from(tile.dimensions.0);
        let bottom = top + f64::from(tile.dimensions.1);
        if ![left, top, right, bottom].into_iter().all(f64::is_finite) {
            return Err(invalid_slide(path, "non-finite MIRAX occupied bounds"));
        }
        bounds = Some(bounds.map_or((left, top, right, bottom), |current| {
            (
                current.0.min(left),
                current.1.min(top),
                current.2.max(right),
                current.3.max(bottom),
            )
        }));
    }
    let Some((left, top, right, bottom)) = bounds else {
        return Ok(None);
    };
    let left = left.floor();
    let top = top.floor();
    let right = right.ceil();
    let bottom = bottom.ceil();
    const I64_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if left < i64::MIN as f64
        || top < i64::MIN as f64
        || right >= I64_MAX_EXCLUSIVE
        || bottom >= I64_MAX_EXCLUSIVE
    {
        return Err(invalid_slide(path, "MIRAX occupied bounds exceed i64"));
    }
    let x = left as i64;
    let y = top as i64;
    let right = right as i64;
    let bottom = bottom as i64;
    let width = u64::try_from(i128::from(right) - i128::from(x))
        .map_err(|_| invalid_slide(path, "invalid MIRAX occupied width"))?;
    let height = u64::try_from(i128::from(bottom) - i128::from(y))
        .map_err(|_| invalid_slide(path, "invalid MIRAX occupied height"))?;
    if width == 0 || height == 0 {
        return Err(invalid_slide(path, "empty MIRAX occupied bounds"));
    }
    Ok(Some((x, y, width, height)))
}

fn assemble_dataset_and_caches(
    cache_config: CacheConfig,
    sources: MiraxIndexSources,
    properties: Properties,
    associated_metadata: HashMap<String, AssociatedImage>,
) -> MiraxSlide {
    let level_builders = sources.hierarchy.level_builders;
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

    let decoded_image_bytes = levels
        .iter()
        .flat_map(|level| level.tiles.iter())
        .map(|tile| {
            u64::from(tile.image.expected_width)
                .saturating_mul(u64::from(tile.image.expected_height))
                .saturating_mul(3)
        })
        .max()
        .unwrap_or(1);
    let associated_image_bytes = associated_metadata
        .values()
        .map(|image| {
            u64::from(image.dimensions.0)
                .saturating_mul(u64::from(image.dimensions.1))
                .saturating_mul(u64::from(image.channels))
        })
        .max()
        .unwrap_or(1);
    let mut private_cache_budget = cache_config.private_cache_budget(2);
    let decoded_cache = PrivateCache::new(private_cache_budget.allocate(decoded_image_bytes));
    let associated_cache = PrivateCache::new(private_cache_budget.allocate(associated_image_bytes));

    let dataset = Dataset {
        id: sources.dataset_id,
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

    MiraxSlide {
        dataset,
        levels,
        associated: sources.associated,
        decoded_images: Mutex::new(decoded_cache),
        associated_cache: Mutex::new(associated_cache),
        open_files: Mutex::new(sources.quickhash_files),
    }
}
