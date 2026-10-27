use super::*;
use crate::core::registry::OpenBudget;

const MAX_DICOM_DIRECTORY_FILES: usize = 1_024;

pub(super) struct DicomSlide {
    pub(super) encoded_unit_bytes: u64,
    pub(super) dataset: Dataset,
    pub(super) levels: Vec<DicomLevel>,
    pub(super) associated: HashMap<String, Arc<DicomImage>>,
}

impl DicomSlide {
    pub(super) fn parse_with_config(
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Self, WsiError> {
        let budget = OpenBudget::new(config.limits);
        let manifest = DicomSeriesManifest::resolve_with_budget(path, budget.as_ref())?;
        Self::from_manifest(path, config, budget.as_ref(), manifest)
    }

    pub(super) fn parse_selected_metadata_with_config(
        path: &Path,
        config: BackendOpenConfig,
        budget: &OpenBudget,
        selected_meta: ParsedDicomMetadata,
    ) -> Result<Self, WsiError> {
        let manifest =
            DicomSeriesManifest::from_selected_metadata_with_budget(path, budget, selected_meta)?;
        Self::from_manifest(path, config, budget, manifest)
    }

    fn from_manifest(
        path: &Path,
        config: BackendOpenConfig,
        budget: &OpenBudget,
        manifest: DicomSeriesManifest,
    ) -> Result<Self, WsiError> {
        let DicomSeriesManifest {
            identity,
            volume_images,
            associated_images,
            unsupported_associated_images,
            source_file_count,
        } = manifest;
        let private_cache_count = volume_images
            .len()
            .saturating_add(associated_images.len())
            .saturating_mul(2);
        let mut private_cache_budget = config
            .cache_config
            .private_cache_budget(private_cache_count);
        let source_icc_profiles = source_icc_profiles(&volume_images)?;
        let level_images = volume_images
            .into_iter()
            .map(|meta| {
                DicomImage::from_metadata_with_private_cache_budget(
                    meta,
                    &mut private_cache_budget,
                    budget,
                )
            })
            .map(|result| result.map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        let mut associated_images = associated_images
            .into_iter()
            .map(|(kind, meta)| {
                DicomImage::from_metadata_with_private_cache_budget(
                    meta,
                    &mut private_cache_budget,
                    budget,
                )
                .map(Arc::new)
                .map(|image| (kind.name().to_string(), image))
            })
            .collect::<Result<Vec<_>, _>>()?;

        if level_images.is_empty() {
            return Err(WsiError::invalid_slide(path, "No pyramid levels found"));
        }

        dedupe_associated(path, &mut associated_images)?;
        let mut levels = build_levels(path, level_images)?;
        validate_uniform_level_bit_depth(path, &levels)?;
        levels.sort_by(|a, b| {
            b.area()
                .cmp(&a.area())
                .then_with(|| b.width.cmp(&a.width))
                .then_with(|| b.height.cmp(&a.height))
        });
        validate_monotonic_levels(path, &levels)?;
        reject_huge_base_only_dicom(path, &levels)?;

        let level0 = levels
            .first()
            .ok_or_else(|| WsiError::invalid_slide(path, "No pyramid levels found"))?
            .clone();

        let quickhash = quickhash_for_series_uid(&identity.series)?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash, "quickhash")?;
        let largest_dimensions = (level0.width, level0.height);
        let public_levels = levels
            .iter()
            .map(|level| Level {
                dimensions: (level.width as u64, level.height as u64),
                downsample: largest_dimensions.0 as f64 / level.width as f64,
                tile_layout: TileLayout::Regular {
                    tile_width: level.tile_width,
                    tile_height: level.tile_height,
                    tiles_across: level.tiles_across as u64,
                    tiles_down: level.tiles_down as u64,
                },
            })
            .collect::<Vec<_>>();

        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "dicom");
        properties.insert("openslide.quickhash-1", quickhash);
        identity.insert_properties(&mut properties);
        properties.insert("dicom.source-file-count", source_file_count.to_string());
        // Source paths that are not UTF-8 cannot be listed as text; their
        // property is omitted rather than failing the open.
        for (level_index, level) in levels.iter().enumerate() {
            let source_files = level
                .parts
                .iter()
                .map(|part| canonicalize_or_fallback(&part.frame_store.path))
                .collect::<std::collections::BTreeSet<_>>();
            if let Ok(source_files) = serde_json::to_string(&source_files) {
                properties.insert(
                    format!("dicom.level.{level_index}.source-files"),
                    source_files,
                );
            }
        }
        for (name, image) in &associated_images {
            if let Some(source_file) = canonicalize_or_fallback(&image.frame_store.path).to_str() {
                properties.insert(format!("dicom.associated.{name}.source-file"), source_file);
            }
        }
        for (kind, reason) in &unsupported_associated_images {
            properties.insert(
                format!("dicom.unsupported-associated.{}", kind.name()),
                reason,
            );
        }
        let (shared_pixel_spacing, shared_objective_lens_power) =
            if level0.pixel_spacing.is_none() || level0.objective_lens_power.is_none() {
                parse_level0_properties(&level0.path).unwrap_or((None, None))
            } else {
                (None, None)
            };
        let level0_pixel_spacing = level0.pixel_spacing.or(shared_pixel_spacing);
        if let Some((mpp_x, mpp_y)) = level0_pixel_spacing {
            properties.insert("openslide.mpp-x", format!("{mpp_x}"));
            properties.insert("openslide.mpp-y", format!("{mpp_y}"));
        }
        let level0_objective_lens_power =
            level0.objective_lens_power.or(shared_objective_lens_power);
        if let Some(objective) = level0_objective_lens_power {
            properties.insert("openslide.objective-power", format!("{objective}"));
        }

        let associated_metadata = associated_images
            .iter()
            .map(|(name, image)| {
                (
                    name.clone(),
                    AssociatedImage {
                        dimensions: (image.width, image.height),
                        sample_type: image.bit_depth.sample_type(),
                        channels: 3,
                        icc_profile: image.icc_profile.clone(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let associated = associated_images.into_iter().collect::<HashMap<_, _>>();

        let mut dataset = Dataset {
            id: dataset_id,
            scenes: vec![Scene {
                id: "s0".into(),
                name: None,
                series: vec![Series {
                    id: "ser0".into(),
                    axes: AxesShape::default(),
                    levels: public_levels,
                    sample_type: level0.bit_depth().sample_type(),
                    channels: vec![],
                }],
            }],
            associated_images: associated_metadata,
            properties,
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        };
        for profile in source_icc_profiles {
            dataset.push_source_icc_profile(profile).map_err(|err| {
                WsiError::DisplayConversion(format!(
                    "failed to add DICOM source ICC profile to dataset: {err}"
                ))
            })?;
        }

        Ok(Self {
            encoded_unit_bytes: config.limits.encoded_unit_bytes(),
            dataset,
            levels,
            associated,
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct DicomLevel {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) tiles_across: u32,
    pub(super) tiles_down: u32,
    pub(super) path: PathBuf,
    pub(super) pixel_spacing: Option<(f64, f64)>,
    pub(super) objective_lens_power: Option<f64>,
    pub(super) parts: Vec<Arc<DicomImage>>,
}

impl DicomLevel {
    pub(super) fn from_image(image: Arc<DicomImage>) -> Self {
        Self {
            width: image.width,
            height: image.height,
            tile_width: image.tile_width,
            tile_height: image.tile_height,
            tiles_across: image.tiles_across,
            tiles_down: image.tiles_down,
            path: image.frame_store.path.clone(),
            pixel_spacing: image.pixel_spacing,
            objective_lens_power: image.objective_lens_power,
            parts: vec![image],
        }
    }

    pub(super) fn area(&self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }

    pub(super) fn is_regular_full_tiling(&self) -> bool {
        self.parts.iter().all(|part| part.is_full_grid())
    }

    pub(super) fn push_part(
        &mut self,
        path: &Path,
        image: Arc<DicomImage>,
    ) -> Result<(), WsiError> {
        if self
            .parts
            .iter()
            .any(|part| part.sop_instance_uid == image.sop_instance_uid)
        {
            return Ok(());
        }
        if self.tile_width != image.tile_width
            || self.tile_height != image.tile_height
            || self.tiles_across != image.tiles_across
            || self.tiles_down != image.tiles_down
            || self.samples_per_pixel() != image.samples_per_pixel
            || self.planar_configuration() != image.planar_configuration
            || self.photometric_interpretation() != image.photometric_interpretation
            || self.bit_depth() != image.bit_depth
        {
            return Err(WsiError::invalid_slide(
                path,
                format!(
                    "DICOM level {}x{} has incompatible split image {}",
                    self.width, self.height, image.sop_instance_uid
                ),
            ));
        }
        self.parts.push(image);
        Ok(())
    }

    pub(super) fn samples_per_pixel(&self) -> u16 {
        self.parts[0].samples_per_pixel
    }

    pub(super) fn planar_configuration(&self) -> Option<u16> {
        self.parts[0].planar_configuration
    }

    pub(super) fn photometric_interpretation(&self) -> &str {
        &self.parts[0].photometric_interpretation
    }

    pub(super) fn bit_depth(&self) -> DicomBitDepth {
        self.parts[0].bit_depth
    }

    pub(super) fn image_for_tile(&self, col: u32, row: u32) -> Option<Arc<DicomImage>> {
        self.parts
            .iter()
            .find(|image| image.frame_index(col, row).is_some())
            .cloned()
    }

    /// The column and row of `req` in this level's grid, or a `TileRead`
    /// error when the tile lies outside it.
    pub(super) fn tile_coordinates(&self, req: &TileRequest) -> Result<(u32, u32), WsiError> {
        checked_dicom_tile_coordinates(
            req.col,
            req.row,
            req.level.get(),
            self.tiles_across,
            self.tiles_down,
        )
    }

    pub(super) fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.tile_coordinates(req)
            .ok()
            .and_then(|(col, row)| self.image_for_tile(col, row))
            .map_or(TileCodecKind::Other, |image| {
                image.transfer_syntax.tile_codec_kind()
            })
    }

    /// Whether a part holds a frame for the requested tile. Tiles outside the
    /// level grid are absent rather than errors.
    pub(super) fn tile_is_present(&self, req: &TileRequest) -> bool {
        self.tile_coordinates(req)
            .is_ok_and(|(col, row)| self.image_for_tile(col, row).is_some())
    }

    pub(super) fn read_tile(
        &self,
        col: i64,
        row: i64,
        level: u32,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let (col_u32, row_u32) =
            checked_dicom_tile_coordinates(col, row, level, self.tiles_across, self.tiles_down)?;
        if let Some(image) = self.image_for_tile(col_u32, row_u32) {
            return image.read_tile(col, row, level, backend);
        }

        let (width, height) = self.actual_tile_dimensions(col_u32, row_u32);
        black_sample_buffer(width, height, self.bit_depth())
    }

    pub(super) fn read_raw_compressed_tile(
        &self,
        col: i64,
        row: i64,
        level: u32,
    ) -> Result<RawCompressedTile, WsiError> {
        let (col_u32, row_u32) =
            checked_dicom_tile_coordinates(col, row, level, self.tiles_across, self.tiles_down)?;
        for image in &self.parts {
            if image.frame_index(col_u32, row_u32).is_some() {
                return image.read_raw_compressed_tile(col, row, level);
            }
        }

        Err(sparse_missing_raw_tile_error(col, row, level))
    }

    pub(super) fn actual_tile_dimensions(&self, col: u32, row: u32) -> (u32, u32) {
        dicom_actual_tile_dimensions(
            self.width,
            self.height,
            self.tile_width,
            self.tile_height,
            col,
            row,
        )
    }
}

pub(super) fn source_icc_profiles(
    metas: &[ParsedDicomMetadata],
) -> Result<Vec<SourceIccProfile>, WsiError> {
    let mut source_icc_profiles = Vec::<(SourceIccProfileIdentity, SourceIccProfile)>::new();
    for profile in metas
        .iter()
        .flat_map(|meta| meta.source_icc_profiles.iter())
    {
        let identity = source_icc_profile_identity(profile);
        if let Some(existing_profile) = source_icc_profiles
            .iter()
            .find(|(existing_identity, _)| existing_identity == &identity)
            .map(|(_, profile)| profile)
        {
            if existing_profile.bytes != profile.bytes {
                return Err(WsiError::DisplayConversion(format!(
                    "DICOM source contains different ICC profiles for the same DICOM optical path key across volume instances ({} vs {})",
                    source_icc_profile_location(existing_profile),
                    source_icc_profile_location(profile)
                )));
            }
            continue;
        }
        let mut profile = profile.clone();
        if profile.key.optical_path.is_some() {
            profile.key.optical_path = Some(
                source_icc_profiles
                    .iter()
                    .filter(|(_, existing_profile)| {
                        existing_profile.key.scene == profile.key.scene
                            && existing_profile.key.series == profile.key.series
                            && existing_profile.key.optical_path.is_some()
                    })
                    .count(),
            );
        }
        source_icc_profiles.push((identity, profile));
    }
    let qualified_scene_series = source_icc_profiles
        .iter()
        .filter(|(_, profile)| profile.key.optical_path.is_some())
        .map(|(_, profile)| (profile.key.scene, profile.key.series))
        .collect::<std::collections::HashSet<_>>();
    source_icc_profiles.retain(|(_, profile)| {
        profile.key.optical_path.is_some()
            || !qualified_scene_series.contains(&(profile.key.scene, profile.key.series))
    });
    Ok(source_icc_profiles
        .into_iter()
        .map(|(_, profile)| profile)
        .collect())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceIccProfileIdentity {
    scene: SceneId,
    series: SeriesId,
    optical_path: Option<SourceIccProfileOpticalPathIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceIccProfileOpticalPathIdentity {
    Identifier(String),
    Index(usize),
}

fn source_icc_profile_identity(profile: &SourceIccProfile) -> SourceIccProfileIdentity {
    let optical_path = profile
        .key
        .optical_path
        .map(|index| match &profile.provenance {
            IccProfileProvenance::DicomOpticalPath {
                optical_path_identifier: Some(identifier),
                ..
            } => SourceIccProfileOpticalPathIdentity::Identifier(identifier.clone()),
            _ => SourceIccProfileOpticalPathIdentity::Index(index),
        });
    SourceIccProfileIdentity {
        scene: profile.key.scene,
        series: profile.key.series,
        optical_path,
    }
}

fn source_icc_profile_location(profile: &SourceIccProfile) -> String {
    match &profile.provenance {
        IccProfileProvenance::DicomOpticalPath {
            sop_instance_uid,
            optical_path_identifier,
            ..
        } => {
            let optical_path = optical_path_identifier
                .as_deref()
                .map(|identifier| format!(" optical path identifier {identifier}"))
                .unwrap_or_default();
            format!("SOP Instance UID {sop_instance_uid}{optical_path}")
        }
        provenance => format!("{provenance:?}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AssociatedKind {
    Label,
    Macro,
    Thumbnail,
}

impl AssociatedKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Macro => "macro",
            Self::Thumbnail => "thumbnail",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageRole {
    Level,
    Associated(AssociatedKind),
    Ignore,
}

/// The series-level identifiers every instance of one slide shares.
pub(super) struct DicomSeriesIdentity {
    pub(super) study: Option<String>,
    pub(super) series: String,
    pub(super) frame_of_reference: Option<String>,
    pub(super) container: Option<String>,
    pub(super) specimen: Option<String>,
    pub(super) barcode: Option<String>,
}

impl DicomSeriesIdentity {
    /// The identity of the first instance, with each optional identifier
    /// taken from whichever instances carry it; instances that disagree on
    /// one are an error.
    pub(super) fn from_metas(path: &Path, metas: &[ParsedDicomMetadata]) -> Result<Self, WsiError> {
        let first = metas
            .first()
            .ok_or_else(|| WsiError::invalid_slide(path, "No DICOM VL WSI objects found"))?;
        Ok(Self {
            series: first.series_instance_uid.clone(),
            study: common_optional_value(path, "StudyInstanceUID", metas, |meta| {
                meta.study_instance_uid.as_deref()
            })?,
            frame_of_reference: common_optional_value(
                path,
                "FrameOfReferenceUID",
                metas,
                |meta| meta.frame_of_reference_uid.as_deref(),
            )?,
            container: common_optional_value(path, "ContainerIdentifier", metas, |meta| {
                meta.container_identifier.as_deref()
            })?,
            specimen: common_optional_value(path, "SpecimenIdentifier", metas, |meta| {
                meta.specimen_identifier.as_deref()
            })?,
            barcode: common_optional_value(path, "BarcodeValue", metas, |meta| {
                meta.barcode_value.as_deref()
            })?,
        })
    }

    /// Adds the `dicom.*` identifier properties, and `openslide.barcode`.
    pub(super) fn insert_properties(&self, properties: &mut Properties) {
        properties.insert("dicom.series-instance-uid", &self.series);
        if let Some(study) = &self.study {
            properties.insert("dicom.study-instance-uid", study);
        }
        if let Some(frame_of_reference) = &self.frame_of_reference {
            properties.insert("dicom.frame-of-reference-uid", frame_of_reference);
        }
        if let Some(container) = &self.container {
            properties.insert("dicom.container-identifier", container);
        }
        if let Some(specimen) = &self.specimen {
            properties.insert("dicom.specimen-identifier", specimen);
        }
        if let Some(barcode) = &self.barcode {
            properties.insert("dicom.barcode-value", barcode);
            properties.insert("openslide.barcode", barcode);
        }
    }
}

pub(super) struct DicomSeriesManifest {
    pub(super) identity: DicomSeriesIdentity,
    pub(super) volume_images: Vec<ParsedDicomMetadata>,
    pub(super) associated_images: Vec<(AssociatedKind, ParsedDicomMetadata)>,
    /// Associated images left out because their pixel format is unsupported,
    /// with the reason.
    pub(super) unsupported_associated_images: Vec<(AssociatedKind, String)>,
    pub(super) source_file_count: usize,
}

impl DicomSeriesManifest {
    pub(super) fn resolve_with_budget(path: &Path, budget: &OpenBudget) -> Result<Self, WsiError> {
        if path.is_dir() {
            Self::from_directory_with_budget(path, budget)
        } else {
            Self::from_selected_file_with_budget(path, budget)
        }
    }

    fn from_selected_file_with_budget(path: &Path, budget: &OpenBudget) -> Result<Self, WsiError> {
        let selected_meta = parse_metadata_object_with_budget(path, budget)?;
        Self::from_selected_metadata_with_budget(path, budget, selected_meta)
    }

    pub(super) fn from_selected_metadata_with_budget(
        path: &Path,
        budget: &OpenBudget,
        selected_meta: ParsedDicomMetadata,
    ) -> Result<Self, WsiError> {
        let selected_series_uid = selected_meta.series_instance_uid.clone();
        let scan_root = path.parent().unwrap_or_else(|| Path::new("."));
        let selected_key = canonicalize_or_fallback(path);
        let mut metas = vec![selected_meta];

        for sibling_path in direct_child_files(scan_root)? {
            if canonicalize_or_fallback(&sibling_path) == selected_key {
                continue;
            }
            let meta = match parse_metadata_object_with_budget(&sibling_path, budget) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.series_instance_uid == selected_series_uid {
                metas.push(meta);
            }
        }

        Self::from_group(path, metas)
    }

    fn from_directory_with_budget(path: &Path, budget: &OpenBudget) -> Result<Self, WsiError> {
        let mut by_series = HashMap::<String, Vec<ParsedDicomMetadata>>::new();
        for child_path in direct_child_files(path)? {
            let meta = match parse_metadata_object_with_budget(&child_path, budget) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            by_series
                .entry(meta.series_instance_uid.clone())
                .or_default()
                .push(meta);
        }

        if by_series.is_empty() {
            return Err(WsiError::UnsupportedFormat(path.display().to_string()));
        }
        if by_series.len() != 1 {
            return Err(WsiError::invalid_slide(
                path,
                format!(
                    "DICOM directory contains {} VL WSI series; select a directory containing exactly one series",
                    by_series.len()
                ),
            ));
        }

        let metas = by_series
            .into_values()
            .next()
            .expect("series map is known to contain one entry");
        Self::from_group(path, metas)
    }

    pub(super) fn from_group(
        path: &Path,
        metas: Vec<ParsedDicomMetadata>,
    ) -> Result<Self, WsiError> {
        let identity = DicomSeriesIdentity::from_metas(path, &metas)?;
        let source_file_count = metas.len();

        for meta in &metas {
            if meta.series_instance_uid != identity.series {
                return Err(WsiError::invalid_slide(
                    path,
                    "DICOM series resolver received mixed SeriesInstanceUID values",
                ));
            }
        }

        let mut volume_images = Vec::new();
        let mut associated_images = Vec::new();
        let mut unsupported_associated_images = Vec::new();
        for mut meta in metas {
            match meta.classify() {
                Ok(ImageRole::Ignore) => {}
                Ok(ImageRole::Level) => volume_images.push(meta),
                Ok(ImageRole::Associated(kind)) => associated_images.push((kind, meta)),
                // An associated image whose pixel format this reader cannot
                // decode (a 16-bit overview, say) does not make the pyramid
                // unreadable; it is left out and named in the properties.
                Err(error) => match meta.role() {
                    ImageRole::Associated(kind) => {
                        unsupported_associated_images.push((kind, error.to_string()));
                    }
                    ImageRole::Level | ImageRole::Ignore => return Err(error),
                },
            }
        }

        Ok(Self {
            identity,
            volume_images,
            associated_images,
            unsupported_associated_images,
            source_file_count,
        })
    }
}

/// Pyramid levels form one series with one sample type, so every level must
/// share its precision. Split parts of one level are already checked when
/// they are merged. Associated images keep their own precision.
fn validate_uniform_level_bit_depth(path: &Path, levels: &[DicomLevel]) -> Result<(), WsiError> {
    let Some(first) = levels.first() else {
        return Ok(());
    };
    match levels
        .iter()
        .find(|level| level.bit_depth() != first.bit_depth())
    {
        Some(mixed) => Err(WsiError::invalid_slide(
            path,
            format!(
                "DICOM series mixes {}-bit and {}-bit pyramid images ({} vs. {})",
                first.bit_depth().bits_stored(),
                mixed.bit_depth().bits_stored(),
                first.parts[0].sop_instance_uid,
                mixed.parts[0].sop_instance_uid
            ),
        )),
        None => Ok(()),
    }
}

pub(super) fn direct_child_files(dir: &Path) -> Result<Vec<PathBuf>, WsiError> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|source| WsiError::io_with_path(dir, source))? {
        let entry = entry.map_err(|source| WsiError::io_with_path(dir, source))?;
        let file_type = entry
            .file_type()
            .map_err(|source| WsiError::io_with_path(&entry.path(), source))?;
        if file_type.is_file() {
            if paths.len() >= MAX_DICOM_DIRECTORY_FILES {
                return Err(WsiError::invalid_slide(
                    dir,
                    format!("DICOM directory exceeds {MAX_DICOM_DIRECTORY_FILES} direct files"),
                ));
            }
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn common_optional_value<F>(
    path: &Path,
    name: &str,
    metas: &[ParsedDicomMetadata],
    value: F,
) -> Result<Option<String>, WsiError>
where
    F: Fn(&ParsedDicomMetadata) -> Option<&str>,
{
    let mut common = None::<String>;
    for meta in metas {
        let Some(actual) = value(meta) else {
            continue;
        };
        match &common {
            Some(expected) if expected != actual => {
                return Err(WsiError::invalid_slide(
                    path,
                    format!(
                        "DICOM series has incompatible {name} values ({expected} vs. {actual})"
                    ),
                ));
            }
            Some(_) => {}
            None => common = Some(actual.to_string()),
        }
    }
    Ok(common)
}

#[cfg(test)]
#[path = "manifest/tests/directory_limit.rs"]
mod directory_limit_tests;
