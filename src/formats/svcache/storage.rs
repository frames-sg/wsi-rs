use super::*;

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
    if metadata_len > 128 * 1024 * 1024 {
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
    let payload_start = 16 + metadata_len;
    Ok((file, payload_start, metadata))
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
    let meta = std::fs::metadata(path)?;
    let modified_unix_nanos = meta
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    Ok(SourceFingerprint {
        path: path.to_string_lossy().to_string(),
        len: meta.len(),
        modified_unix_nanos,
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
        self.len == other.len && self.modified_unix_nanos == other.modified_unix_nanos
    }
}
