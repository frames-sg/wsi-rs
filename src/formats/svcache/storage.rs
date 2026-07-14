use super::*;
use std::collections::HashSet;
use std::sync::Arc;

const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCENES: usize = 1_024;
const MAX_SERIES: usize = 4_096;
const MAX_LEVELS: usize = 16_384;
const MAX_CHANNELS_PER_SERIES: usize = 64;
const MAX_ASSOCIATED_IMAGES: usize = 1_024;
const MAX_PROPERTIES: usize = 100_000;
const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_TILES_PER_LEVEL: u64 = 16 * 1024 * 1024;
const MAX_TILE_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DECODED_TILE_BYTES: u64 = 512 * 1024 * 1024;

pub(super) fn read_svcache(path: &Path) -> Result<(File, u64, SvcacheMetadata), WsiError> {
    let mut file = File::open(path)?;
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(WsiError::UnsupportedFormat(format!(
            "{} is not an .svcache file",
            path.display()
        )));
    }
    let mut len = [0_u8; 8];
    file.read_exact(&mut len)?;
    let metadata_len = u64::from_le_bytes(len);
    if metadata_len > MAX_METADATA_BYTES {
        return Err(WsiError::InvalidSlide {
            path: path.into(),
            message: "svcache metadata is too large".into(),
        });
    }
    let mut metadata_bytes = vec![0_u8; metadata_len as usize];
    file.read_exact(&mut metadata_bytes)?;
    let metadata: SvcacheMetadata =
        serde_json::from_slice(&metadata_bytes).map_err(|err| WsiError::InvalidSlide {
            path: path.into(),
            message: format!("parse svcache metadata: {err}"),
        })?;
    if metadata.schema_version != SCHEMA_VERSION {
        return Err(WsiError::UnsupportedFormat(format!(
            "unsupported svcache schema {}",
            metadata.schema_version
        )));
    }
    let payload_start = 16_u64
        .checked_add(metadata_len)
        .ok_or_else(|| invalid_svcache(path, "svcache payload start overflow"))?;
    let file_len = file.metadata()?.len();
    validate_svcache_metadata(path, file_len, payload_start, &metadata)?;
    Ok((file, payload_start, metadata))
}

fn invalid_svcache(path: &Path, message: impl Into<String>) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn validate_svcache_metadata(
    path: &Path,
    file_len: u64,
    payload_start: u64,
    metadata: &SvcacheMetadata,
) -> Result<(), WsiError> {
    if payload_start > file_len {
        return Err(invalid_svcache(path, "svcache metadata extends past EOF"));
    }
    validate_string(path, "source path", &metadata.source.path)?;
    if metadata.scenes.len() > MAX_SCENES {
        return Err(invalid_svcache(path, "svcache has too many scenes"));
    }
    if metadata.associated.len() > MAX_ASSOCIATED_IMAGES {
        return Err(invalid_svcache(
            path,
            "svcache has too many associated images",
        ));
    }
    if metadata.properties.len() > MAX_PROPERTIES {
        return Err(invalid_svcache(path, "svcache has too many properties"));
    }

    let mut property_names = HashSet::new();
    for (name, value) in &metadata.properties {
        validate_string(path, "property name", name)?;
        validate_string(path, "property value", value)?;
        if !property_names.insert(name.as_str()) {
            return Err(invalid_svcache(
                path,
                format!("duplicate svcache property {name}"),
            ));
        }
    }

    let payload_len = file_len - payload_start;
    let mut payload_ranges = Vec::new();
    let mut series_count = 0usize;
    let mut level_count = 0usize;
    let mut scene_ids = HashSet::new();
    for scene in &metadata.scenes {
        validate_string(path, "scene id", &scene.id)?;
        if let Some(name) = &scene.name {
            validate_string(path, "scene name", name)?;
        }
        if !scene_ids.insert(scene.id.as_str()) {
            return Err(invalid_svcache(
                path,
                format!("duplicate svcache scene id {}", scene.id),
            ));
        }
        series_count = series_count
            .checked_add(scene.series.len())
            .ok_or_else(|| invalid_svcache(path, "svcache series count overflow"))?;
        if series_count > MAX_SERIES {
            return Err(invalid_svcache(path, "svcache has too many series"));
        }
        let mut series_ids = HashSet::new();
        for series in &scene.series {
            validate_string(path, "series id", &series.id)?;
            if !series_ids.insert(series.id.as_str()) {
                return Err(invalid_svcache(
                    path,
                    format!("duplicate svcache series id {}", series.id),
                ));
            }
            if series.axes.z == 0 || series.axes.c == 0 || series.axes.t == 0 {
                return Err(invalid_svcache(
                    path,
                    "svcache series axis extents must be positive",
                ));
            }
            if series.channels.len() > MAX_CHANNELS_PER_SERIES {
                return Err(invalid_svcache(
                    path,
                    "svcache series channel count is invalid",
                ));
            }
            for channel in &series.channels {
                if let Some(name) = &channel.name {
                    validate_string(path, "channel name", name)?;
                }
            }
            level_count = level_count
                .checked_add(series.levels.len())
                .ok_or_else(|| invalid_svcache(path, "svcache level count overflow"))?;
            if level_count > MAX_LEVELS {
                return Err(invalid_svcache(path, "svcache has too many levels"));
            }
            for level in &series.levels {
                validate_level(
                    path,
                    payload_len,
                    metadata.complete,
                    level,
                    &mut payload_ranges,
                )?;
            }
        }
    }

    let mut associated_names = HashSet::new();
    for associated in &metadata.associated {
        validate_string(path, "associated image name", &associated.name)?;
        if associated.name.is_empty() || !associated_names.insert(associated.name.as_str()) {
            return Err(invalid_svcache(
                path,
                format!(
                    "duplicate or empty svcache associated image name {}",
                    associated.name
                ),
            ));
        }
        if associated.dimensions != (associated.tile.width, associated.tile.height) {
            return Err(invalid_svcache(
                path,
                format!(
                    "svcache associated image {} dimensions do not match its tile",
                    associated.name
                ),
            ));
        }
        validate_tile(path, payload_len, &associated.tile, &mut payload_ranges)?;
    }

    payload_ranges.sort_unstable_by_key(|range| range.0);
    for pair in payload_ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(invalid_svcache(path, "svcache payload ranges overlap"));
        }
    }
    Ok(())
}

fn validate_level(
    path: &Path,
    payload_len: u64,
    complete: bool,
    level: &LevelMeta,
    payload_ranges: &mut Vec<(u64, u64)>,
) -> Result<(), WsiError> {
    if level.dimensions.0 == 0
        || level.dimensions.1 == 0
        || level.tile_width == 0
        || level.tile_height == 0
        || !level.downsample.is_finite()
        || level.downsample <= 0.0
    {
        return Err(invalid_svcache(path, "svcache level geometry is invalid"));
    }
    let expected_across = level.dimensions.0.div_ceil(u64::from(level.tile_width));
    let expected_down = level.dimensions.1.div_ceil(u64::from(level.tile_height));
    if level.tiles_across != expected_across || level.tiles_down != expected_down {
        return Err(invalid_svcache(
            path,
            "svcache level tile grid does not match dimensions",
        ));
    }
    let tile_count = level
        .tiles_across
        .checked_mul(level.tiles_down)
        .ok_or_else(|| invalid_svcache(path, "svcache level tile count overflow"))?;
    if tile_count > MAX_TILES_PER_LEVEL {
        return Err(invalid_svcache(
            path,
            "svcache level tile count exceeds safety limit",
        ));
    }
    if !level.tiles.is_empty() && !level.sparse_tiles.is_empty() {
        return Err(invalid_svcache(
            path,
            "svcache level mixes dense and sparse tile indexes",
        ));
    }
    if !level.tiles.is_empty() {
        if u64::try_from(level.tiles.len()).ok() != Some(tile_count) {
            return Err(invalid_svcache(
                path,
                "svcache dense tile index has incorrect length",
            ));
        }
        if complete && level.tiles.iter().any(Option::is_none) {
            return Err(invalid_svcache(
                path,
                "complete svcache contains an empty dense tile slot",
            ));
        }
        for tile in level.tiles.iter().flatten() {
            validate_tile(path, payload_len, tile, payload_ranges)?;
        }
    } else {
        if complete && u64::try_from(level.sparse_tiles.len()).ok() != Some(tile_count) {
            return Err(invalid_svcache(
                path,
                "complete svcache does not contain every tile",
            ));
        }
        let mut previous = None;
        for entry in &level.sparse_tiles {
            if entry.index >= tile_count || previous.is_some_and(|index| entry.index <= index) {
                return Err(invalid_svcache(
                    path,
                    "svcache sparse tile indexes are unordered, duplicated, or out of range",
                ));
            }
            previous = Some(entry.index);
            validate_tile(path, payload_len, &entry.tile, payload_ranges)?;
        }
    }
    Ok(())
}

fn validate_tile(
    path: &Path,
    payload_file_len: u64,
    tile: &TileMeta,
    payload_ranges: &mut Vec<(u64, u64)>,
) -> Result<(), WsiError> {
    if tile.width == 0 || tile.height == 0 {
        return Err(invalid_svcache(
            path,
            "svcache tile dimensions must be positive",
        ));
    }
    let expected_channels = match tile.color_space {
        ColorSpaceMeta::Rgb => 3,
        ColorSpaceMeta::Rgba => 4,
        ColorSpaceMeta::Grayscale => 1,
    };
    if tile.channels != expected_channels {
        return Err(invalid_svcache(
            path,
            "svcache tile channel count does not match color space",
        ));
    }
    let decoded_len = u64::from(tile.width)
        .checked_mul(u64::from(tile.height))
        .and_then(|pixels| pixels.checked_mul(u64::from(tile.channels)))
        .ok_or_else(|| invalid_svcache(path, "svcache decoded tile length overflow"))?;
    if decoded_len > MAX_DECODED_TILE_BYTES
        || usize::try_from(decoded_len).ok() != Some(tile.decoded_len)
    {
        return Err(invalid_svcache(
            path,
            "svcache decoded tile length is invalid",
        ));
    }
    if tile.payload_len == 0 || tile.payload_len > MAX_TILE_PAYLOAD_BYTES {
        return Err(invalid_svcache(
            path,
            "svcache encoded tile length is invalid",
        ));
    }
    let payload_end = tile
        .payload_offset
        .checked_add(tile.payload_len)
        .ok_or_else(|| invalid_svcache(path, "svcache tile payload range overflow"))?;
    if payload_end > payload_file_len {
        return Err(invalid_svcache(
            path,
            "svcache tile payload extends past EOF",
        ));
    }
    if tile.sha256.len() != 64
        || !tile
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_svcache(path, "svcache tile checksum is invalid"));
    }
    payload_ranges.push((tile.payload_offset, payload_end));
    Ok(())
}

fn validate_string(path: &Path, label: &str, value: &str) -> Result<(), WsiError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(invalid_svcache(
            path,
            format!("svcache {label} exceeds string length limit"),
        ));
    }
    Ok(())
}

pub(super) fn is_fresh_svcache(cache_path: &Path, source_path: &Path) -> Result<bool, WsiError> {
    let (_, _, metadata) = read_svcache(cache_path)?;
    Ok(metadata.complete && metadata.source == fingerprint_source(source_path)?)
}

pub fn svcache_matches_source(cache_path: &Path, source_path: &Path) -> Result<bool, WsiError> {
    let (_, _, metadata) = read_svcache(cache_path)?;
    Ok(metadata.source == fingerprint_source(source_path)?)
}

pub(super) fn fingerprint_source(path: &Path) -> Result<SourceFingerprint, WsiError> {
    const SAMPLE_BYTES: u64 = 64 * 1024;

    let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let before = file.metadata().map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    if !before.is_file() {
        return Err(invalid_svcache(
            path,
            "svcache source fingerprint requires a regular file",
        ));
    }
    let modified_unix_nanos = before
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    let len = before.len();
    let mut offsets = vec![
        0,
        len / 4,
        len / 2,
        (len / 4).saturating_mul(3),
        len.saturating_sub(SAMPLE_BYTES),
    ];
    offsets.sort_unstable();
    offsets.dedup();
    let mut digest = Sha256::new();
    digest.update(len.to_le_bytes());
    let mut buffer = vec![0u8; usize::try_from(SAMPLE_BYTES).unwrap_or(0)];
    for offset in offsets {
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        let to_read = usize::try_from((len - offset).min(SAMPLE_BYTES)).unwrap_or(0);
        file.read_exact(&mut buffer[..to_read])
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        digest.update(offset.to_le_bytes());
        digest.update(&buffer[..to_read]);
    }
    let after = file.metadata().map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let after_modified = after
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    if after.len() != len || after_modified != modified_unix_nanos {
        return Err(invalid_svcache(
            path,
            "svcache source changed while computing its fingerprint",
        ));
    }
    Ok(SourceFingerprint {
        path: canonical_path.to_string_lossy().into_owned(),
        len,
        modified_unix_nanos,
        sample_sha256: hex_encode(&digest.finalize()),
    })
}

pub(super) fn dataset_from_metadata(path: &Path, metadata: &SvcacheMetadata) -> Dataset {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(metadata.source.path.as_bytes());
    let digest = hasher.finalize();
    let mut id_bytes = [0_u8; 16];
    id_bytes.copy_from_slice(&digest[..16]);
    let mut properties = Properties::new();
    for (key, value) in &metadata.properties {
        properties.insert(key.clone(), value.clone());
    }
    properties.insert("openslide.vendor", "svcache");

    Dataset {
        id: DatasetId::new(u128::from_le_bytes(id_bytes)),
        scenes: metadata
            .scenes
            .iter()
            .map(|scene| Scene {
                id: scene.id.clone(),
                name: scene.name.clone(),
                series: scene
                    .series
                    .iter()
                    .map(|series| Series {
                        id: series.id.clone(),
                        axes: AxesShape {
                            z: series.axes.z,
                            c: series.axes.c,
                            t: series.axes.t,
                        },
                        levels: series
                            .levels
                            .iter()
                            .map(|level| Level {
                                dimensions: level.dimensions,
                                downsample: level.downsample,
                                tile_layout: TileLayout::Regular {
                                    tile_width: level.tile_width,
                                    tile_height: level.tile_height,
                                    tiles_across: level.tiles_across,
                                    tiles_down: level.tiles_down,
                                },
                            })
                            .collect(),
                        sample_type: SampleType::Uint8,
                        channels: series
                            .channels
                            .iter()
                            .map(|channel| ChannelInfo {
                                name: channel.name.clone(),
                                color: channel.color,
                                excitation_nm: None,
                                emission_nm: None,
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
        associated_images: metadata
            .associated
            .iter()
            .map(|assoc| {
                (
                    assoc.name.clone(),
                    AssociatedImage {
                        dimensions: assoc.dimensions,
                        sample_type: SampleType::Uint8,
                        channels: assoc.tile.channels,
                    },
                )
            })
            .collect(),
        properties,
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

impl TryFrom<&ColorSpace> for ColorSpaceMeta {
    type Error = WsiError;

    fn try_from(value: &ColorSpace) -> Result<Self, Self::Error> {
        match value {
            ColorSpace::Rgb => Ok(Self::Rgb),
            ColorSpace::Rgba => Ok(Self::Rgba),
            ColorSpace::Grayscale => Ok(Self::Grayscale),
            other => Err(WsiError::UnsupportedFormat(format!(
                ".svcache builder does not support {:?} display tiles",
                other
            ))),
        }
    }
}

impl From<ColorSpaceMeta> for ColorSpace {
    fn from(value: ColorSpaceMeta) -> Self {
        match value {
            ColorSpaceMeta::Rgb => ColorSpace::Rgb,
            ColorSpaceMeta::Rgba => ColorSpace::Rgba,
            ColorSpaceMeta::Grayscale => ColorSpace::Grayscale,
        }
    }
}

impl PartialEq for SourceFingerprint {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.len == other.len
            && self.modified_unix_nanos == other.modified_unix_nanos
            && self.sample_sha256 == other.sample_sha256
    }
}
