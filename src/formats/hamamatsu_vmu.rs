//! Hamamatsu VMU key files and uncompressed, native RGB16 NGR companions.
mod ngr;

use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::limits::read_file_bounded;
use crate::core::registry::{
    BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe, DatasetReader, FormatProbe,
    ManagedSlideReader, OpenBudget, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, jpeg_dimensions, JpegDecodeJob};
use crate::error::WsiError;
use crate::formats::companion_path::resolve_companion_file;
use crate::formats::ini::{parse_ini_file_with_budget, ParsedIni};
use crate::properties::Properties;
use ngr::Ngr;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

const GROUP: &str = "Uncompressed Virtual Microscope Specimen";

pub(crate) struct HamamatsuVmuBackend;

fn invalid(path: &Path, message: impl Into<String>) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn key_file(path: &Path, budget: &OpenBudget) -> Result<ParsedIni, WsiError> {
    parse_ini_file_with_budget(
        path,
        64 << 10,
        |p| invalid(p, "VMU key file too large"),
        false,
        budget,
    )
}

impl FormatProbe for HamamatsuVmuBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        self.probe_with_config(path, BackendOpenConfig::deterministic())
    }
}
impl ConfiguredFormatProbe for HamamatsuVmuBackend {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        let budget = OpenBudget::new(config.limits);
        Ok(match key_file(path, budget.as_ref()) {
            Ok(ini) if ini.groups.contains_key(GROUP) => {
                ProbeResult::detected("hamamatsu", ProbeConfidence::Definite)
            }
            _ => ProbeResult::not_detected("hamamatsu"),
        })
    }
}
impl DatasetReader for HamamatsuVmuBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        Ok(Box::new(VmuReader::open(
            path,
            BackendOpenConfig::deterministic(),
        )?))
    }
}
impl ConfiguredDatasetReader for HamamatsuVmuBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        Ok(Box::new(VmuReader::open(path, config)?))
    }
}

struct VmuReader {
    dataset: Dataset,
    levels: [Ngr; 2],
    macro_bytes: Option<Vec<u8>>,
    encoded_limit: u64,
}

impl VmuReader {
    fn open(path: &Path, config: BackendOpenConfig) -> Result<Self, WsiError> {
        let budget = OpenBudget::new(config.limits);
        let group = key_file(path, budget.as_ref())?
            .groups
            .remove(GROUP)
            .ok_or_else(|| invalid(path, "missing VMU specimen group"))?;
        if group
            .get("BitsPerPixel")
            .and_then(|s| s.parse::<u32>().ok())
            != Some(36)
            || group.get("PixelOrder").map(String::as_str) != Some("RGB")
        {
            return Err(invalid(
                path,
                "VMU requires BitsPerPixel=36 and PixelOrder=RGB",
            ));
        }
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        let mut image = None;
        for (key, value) in &group {
            if !key.starts_with("ImageFile") {
                continue;
            }
            let (layer, col, row) = image_coordinates(path, key)?;
            if layer != 0 {
                continue;
            }
            if col != 0 || row != 0 || image.is_some() {
                return Err(invalid(
                    path,
                    "VMU requires exactly one base image at (0,0)",
                ));
            }
            image = Some(resolve_companion_file(path, root, value)?);
        }
        let image = image.ok_or_else(|| invalid(path, "missing VMU ImageFile"))?;
        let map = resolve_companion_file(
            path,
            root,
            group
                .get("MapFile")
                .ok_or_else(|| invalid(path, "missing VMU MapFile"))?,
        )?;
        let levels = [Ngr::open(&image)?, Ngr::open(&map)?];
        if levels[1].width > levels[0].width || levels[1].height > levels[0].height {
            return Err(invalid(path, "VMU map dimensions exceed base image"));
        }
        let mut quickhash = Quickhash1::new();
        quickhash.hash_file(path)?;
        quickhash.hash_file(&map)?;
        let hash = quickhash
            .finish()
            .ok_or_else(|| invalid(path, "VMU quickhash unavailable"))?;
        let id = dataset_id_from_quickhash(path, &hash, "VMU quickhash")?;
        let properties = properties(&group, &levels[0], hash);
        let mut associated_images = HashMap::new();
        let macro_bytes = load_macro(path, root, &group, budget.as_ref())?.map(|image| {
            associated_images.insert("macro".into(), image.info);
            image.bytes
        });
        let downsample = (f64::from(levels[0].width) / f64::from(levels[1].width)
            + f64::from(levels[0].height) / f64::from(levels[1].height))
            / 2.0;
        let dataset = Dataset {
            id,
            scenes: vec![Scene {
                id: "s0".into(),
                name: None,
                series: vec![Series {
                    id: "ser0".into(),
                    axes: AxesShape::default(),
                    levels: vec![levels[0].level(1.0), levels[1].level(downsample)],
                    sample_type: SampleType::Uint16,
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
            macro_bytes,
            encoded_limit: config.limits.encoded_unit_bytes(),
        })
    }

    fn resolve(&self, req: &TileRequest) -> Result<&Ngr, WsiError> {
        if req.scene.get() != 0 {
            return Err(WsiError::SceneOutOfRange {
                index: req.scene.get(),
                count: 1,
            });
        }
        if req.series.get() != 0 {
            return Err(WsiError::SeriesOutOfRange {
                index: req.series.get(),
                count: 1,
            });
        }
        if req.plane.get() != PlaneSelection::default() {
            return Err(WsiError::Unsupported {
                reason: "VMU exposes only focal plane zero and interleaved RGB".into(),
            });
        }
        let level = self
            .levels
            .get(req.level.get() as usize)
            .ok_or(WsiError::LevelOutOfRange {
                level: req.level.get(),
                count: 2,
            })?;
        level.tile_dimensions(req)?;
        Ok(level)
    }
}

fn image_coordinates(path: &Path, key: &str) -> Result<(u32, u32, u32), WsiError> {
    if key == "ImageFile" {
        return Ok((0, 0, 0));
    }
    let suffix = key
        .strip_prefix("ImageFile(")
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| invalid(path, "invalid VMU ImageFile coordinates"))?;
    let fields = suffix
        .split(',')
        .map(|s| s.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid(path, "invalid VMU ImageFile coordinate integer"))?;
    match fields.as_slice() {
        [x, y] => Ok((0, *x, *y)),
        [z, x, y] => Ok((*z, *x, *y)),
        _ => Err(invalid(
            path,
            "VMU ImageFile needs two or three coordinates",
        )),
    }
}

fn properties(group: &HashMap<String, String>, base: &Ngr, hash: String) -> Properties {
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "hamamatsu");
    properties.insert("openslide.quickhash-1", hash);
    for (key, value) in group {
        properties.insert(format!("hamamatsu.{key}"), value.clone());
    }
    if let Some(lens) = group.get("SourceLens") {
        properties.insert("openslide.objective-power", lens.clone());
    }
    for (key, output, pixels) in [
        ("PhysicalWidth", "openslide.mpp-x", base.width),
        ("PhysicalHeight", "openslide.mpp-y", base.height),
    ] {
        if let Some(nm) = group
            .get(key)
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
        {
            properties.insert(
                output,
                (nm as f64 / (1000.0 * f64::from(pixels))).to_string(),
            );
        }
    }
    properties
}

struct MacroImage {
    bytes: Vec<u8>,
    info: AssociatedImage,
}

fn load_macro(
    path: &Path,
    root: &Path,
    group: &HashMap<String, String>,
    budget: &OpenBudget,
) -> Result<Option<MacroImage>, WsiError> {
    let Some(value) = group.get("MacroImage") else {
        return Ok(None);
    };
    let macro_path = resolve_companion_file(path, root, value)?;
    // Retained compressed bytes count against the metadata budget at open.
    let limit = budget
        .limits()
        .encoded_unit_bytes()
        .min(budget.limits().aggregate_metadata_bytes());
    let length = std::fs::metadata(&macro_path)?.len();
    budget.retain_metadata(length)?;
    let bytes =
        read_file_bounded(&macro_path, limit.min(length), "VMU macro JPEG").map_err(|source| {
            WsiError::IoWithPath {
                source: std::sync::Arc::new(source),
                path: macro_path,
            }
        })?;
    let info = AssociatedImage {
        dimensions: jpeg_dimensions(&bytes)?,
        sample_type: SampleType::Uint8,
        channels: 3,
        icc_profile: Vec::new(),
    };
    Ok(Some(MacroImage { bytes, info }))
}

impl SlideReader for VmuReader {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.resolve(req)?.read_tile(req, self.encoded_limit)
    }
    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let bytes = self
            .macro_bytes
            .as_ref()
            .filter(|_| name == "macro")
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        crate::core::batch::exactly_one(
            decode_batch_jpeg(&[JpegDecodeJob {
                data: Cow::Borrowed(bytes),
                tables: None,
                expected_width: 0,
                expected_height: 0,
                color_transform: j2k_jpeg::ColorTransform::Auto,
                force_dimensions: false,
                requested_size: None,
            }]),
            "VMU macro JPEG decode",
        )?
    }
}
impl ManagedSlideReader for VmuReader {
    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        let (_, _, w, h) = self.resolve(req)?.tile_dimensions(req)?;
        Ok(u64::from(w) * u64::from(h) * 6)
    }
    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        reqs.iter().try_fold(0u64, |total, req| {
            Ok(total.saturating_add(self.tile_encoded_upper_bound(req)?))
        })
    }
    fn display_tile_encoded_upper_bound(&self, _req: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(256 * 64 * 6)
    }
    fn associated_encoded_upper_bound(&self, name: &str) -> Result<u64, WsiError> {
        Ok(self
            .macro_bytes
            .as_ref()
            .filter(|_| name == "macro")
            .map_or(0, |b| b.len() as u64))
    }
    fn region_fastpath_encoded_upper_bound(&self, _req: &RegionRequest) -> Result<u64, WsiError> {
        Ok(0)
    }
}
