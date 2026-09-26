#[cfg(any(feature = "metal", feature = "cuda"))]
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::limits::{read_to_end_bounded, MAX_COMPRESSED_INPUT_BYTES};
use crate::core::registry::{
    BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe, DatasetReader, FormatProbe,
    ManagedSlideReader, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::{
    AssociatedImage, AxesShape, ChannelInfo, Compression, CpuTile, Dataset,
    EncodedTilePhotometricInterpretation, Level, PlaneSelection, RawCompressedTile, RegionRequest,
    SampleType, Scene, Series, TileCodecKind, TileLayout, TileRequest, TileViewRequest,
};
use crate::decode::jp2k::{
    decode_jp2k_reduced_to_sample_buffer, decode_jp2k_to_sample_buffer,
    jp2k_decodable_reduction_levels, reduced_jp2k_dimensions, Jp2kColorSpace,
};
use crate::decode::jp2k_codestream::{parse_codestream_header, validate_pixel_contract};
use crate::error::WsiError;
use crate::properties::Properties;

#[cfg(test)]
mod tests;

const MARKER_SOC_BYTES: [u8; 2] = [0xFF, 0x4F];
/// Probe vendor and `openslide.vendor`; also salts the dataset quickhash.
const VENDOR: &str = "raw-jp2k";

pub(crate) struct RawJp2kBackend;

impl FormatProbe for RawJp2kBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            return Ok(ProbeResult::not_detected(VENDOR));
        };
        if !matches!(extension.to_ascii_lowercase().as_str(), "j2k" | "j2c") {
            return Ok(ProbeResult::not_detected(VENDOR));
        }

        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => return Ok(ProbeResult::not_detected(VENDOR)),
        };
        let mut magic = [0u8; 2];
        if file.read_exact(&mut magic).is_err() || magic != MARKER_SOC_BYTES {
            return Ok(ProbeResult::not_detected(VENDOR));
        }

        Ok(ProbeResult::detected(VENDOR, ProbeConfidence::Definite))
    }
}

impl ConfiguredFormatProbe for RawJp2kBackend {}

impl DatasetReader for RawJp2kBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let reader = self.open_with_config(path, BackendOpenConfig::deterministic())?;
        Ok(reader)
    }
}

impl ConfiguredDatasetReader for RawJp2kBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        let file = File::open(path).map_err(|source| WsiError::IoWithPath {
            source: std::sync::Arc::new(source),
            path: path.to_path_buf(),
        })?;
        let max_input = MAX_COMPRESSED_INPUT_BYTES.min(config.limits.encoded_unit_bytes());
        let declared_len = file
            .metadata()
            .map_err(|source| WsiError::IoWithPath {
                source: std::sync::Arc::new(source),
                path: path.to_path_buf(),
            })?
            .len();
        if declared_len > max_input {
            return Err(WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested: declared_len,
                limit: max_input,
            });
        }
        let data = read_to_end_bounded(file, max_input, "raw JP2K input").map_err(|source| {
            WsiError::IoWithPath {
                source: std::sync::Arc::new(source),
                path: path.to_path_buf(),
            }
        })?;
        let header = parse_codestream_header(&data)?;
        validate_pixel_contract(&header)?;
        let dimensions = (header.image_width, header.image_height);
        let reader = RawJp2kReader {
            dataset: dataset_for_codestream(
                path,
                &data,
                dimensions,
                jp2k_decodable_reduction_levels(&data)?,
            )?,
            data,
            width: header.image_width,
            height: header.image_height,
        };
        Ok(Box::new(reader))
    }
}

/// Each decodable wavelet reduction is exposed as a pyramid level. Reduced
/// levels are decoded from the codestream's lower resolutions on demand.
fn dataset_for_codestream(
    path: &Path,
    data: &[u8],
    dimensions: (u32, u32),
    reduction_levels: u8,
) -> Result<Dataset, WsiError> {
    let levels = (0..=reduction_levels)
        .map(|reduction| {
            let (width, height) = reduced_jp2k_dimensions(dimensions, reduction)?;
            Ok(Level {
                dimensions: (u64::from(width), u64::from(height)),
                // Reduced pixels cover exact 2^k source blocks, so this is not
                // the rounded width ratio.
                downsample: f64::from(1_u32 << reduction),
                tile_layout: TileLayout::Regular {
                    tile_width: width,
                    tile_height: height,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            })
        })
        .collect::<Result<Vec<_>, WsiError>>()?;
    let quickhash = raw_codestream_quickhash(path, data)?;
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", VENDOR);
    properties.insert("openslide.quickhash-1", quickhash.as_str());
    Ok(Dataset {
        id: dataset_id_from_quickhash(path, &quickhash, "raw JP2K quickhash")?,
        scenes: vec![Scene {
            id: "raw-jp2k".into(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            series: vec![Series {
                id: "0".into(),
                axes: AxesShape::default(),
                levels,
                sample_type: SampleType::Uint8,
                channels: rgb_channels(),
            }],
        }],
        associated_images: HashMap::<String, AssociatedImage>::new(),
        properties,
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    })
}

fn raw_codestream_quickhash(path: &Path, data: &[u8]) -> Result<String, WsiError> {
    let mut hasher = Quickhash1::new();
    hasher.hash_string(VENDOR);
    hasher.hash_string(&path.display().to_string());
    hasher.update(data);
    hasher
        .finish()
        .ok_or_else(|| WsiError::Jp2k("raw JP2K dataset hash disabled".into()))
}

fn rgb_channels() -> Vec<ChannelInfo> {
    vec![
        ChannelInfo {
            name: Some("R".into()),
            color: Some([255, 0, 0]),
            excitation_nm: None,
            emission_nm: None,
        },
        ChannelInfo {
            name: Some("G".into()),
            color: Some([0, 255, 0]),
            excitation_nm: None,
            emission_nm: None,
        },
        ChannelInfo {
            name: Some("B".into()),
            color: Some([0, 0, 255]),
            excitation_nm: None,
            emission_nm: None,
        },
    ]
}

struct RawJp2kReader {
    dataset: Dataset,
    data: Vec<u8>,
    width: u32,
    height: u32,
}

impl RawJp2kReader {
    fn level_count(&self) -> usize {
        self.dataset.scenes[0].series[0].levels.len()
    }

    fn validate_request(&self, req: &TileRequest) -> Result<(), WsiError> {
        if req.scene.get() != 0
            || req.series.get() != 0
            || req.level.get() as usize >= self.level_count()
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "raw JP2K source has one scene, one series, and {} levels",
                    self.level_count()
                ),
            });
        }
        if req.plane.get() != PlaneSelection::default() {
            return Err(WsiError::Unsupported {
                reason: "raw JP2K source has only the default plane".into(),
            });
        }
        if req.col != 0 || req.row != 0 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "raw JP2K source has exactly one tile at (0, 0)".into(),
            });
        }
        Ok(())
    }

    /// Only full resolution is stored as a codestream; reduced levels are
    /// decoded from it on the CPU.
    fn validate_full_resolution_request(
        &self,
        req: &TileRequest,
        operation: &str,
    ) -> Result<(), WsiError> {
        self.validate_request(req)?;
        if req.level.get() != 0 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw JP2K {operation} is available at full resolution only; reduced levels are decoded on the CPU"
                ),
            });
        }
        Ok(())
    }
}

impl SlideReader for RawJp2kReader {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        TileCodecKind::Jp2k
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.validate_request(req)?;
        let reduction = u8::try_from(req.level.get()).map_err(|_| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "raw JP2K reduction level exceeds the codestream ladder".into(),
        })?;
        if reduction == 0 {
            decode_jp2k_to_sample_buffer(&self.data, self.width, self.height, Jp2kColorSpace::Rgb)
        } else {
            decode_jp2k_reduced_to_sample_buffer(&self.data, reduction, Jp2kColorSpace::Rgb)
        }
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        for request in reqs {
            self.validate_full_resolution_request(request, "device decode")?;
        }
        let jobs = reqs
            .iter()
            .map(|_| crate::decode::jp2k::Jp2kDecodeJob {
                data: Cow::Borrowed(self.data.as_slice()),
                expected_width: self.width,
                expected_height: self.height,
                rgb_color_space: true,
                backend: j2k_core::BackendRequest::Metal,
            })
            .collect::<Vec<_>>();
        crate::decode::jp2k::decode_batch_jp2k_metal(&jobs, sessions)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tiles| {
                crate::core::batch::expect_exact_count(tiles, reqs.len(), "raw JP2K Metal batch")
            })
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        for request in reqs {
            self.validate_full_resolution_request(request, "device decode")?;
        }
        let jobs = reqs
            .iter()
            .map(|_| crate::decode::jp2k::Jp2kDecodeJob {
                data: Cow::Borrowed(self.data.as_slice()),
                expected_width: self.width,
                expected_height: self.height,
                rgb_color_space: true,
                backend: j2k_core::BackendRequest::Cuda,
            })
            .collect::<Vec<_>>();
        crate::decode::jp2k::decode_batch_jp2k_cuda(&jobs, sessions)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tiles| {
                crate::core::batch::expect_exact_count(tiles, reqs.len(), "raw JP2K CUDA batch")
            })
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.validate_full_resolution_request(req, "compressed passthrough")?;
        Ok(RawCompressedTile::builder(Compression::Jp2kRgb)
            .dimensions(self.width, self.height)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
            .data(self.data.clone())
            .build()?)
    }
}

impl ManagedSlideReader for RawJp2kReader {
    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn prepare_adaptive_jp2k(
        &self,
        reqs: &[TileRequest],
        workers: usize,
        control: Option<&crate::ReadControl>,
    ) -> Option<Result<crate::decode::jp2k::PreparedJp2kBatch, WsiError>> {
        // Device routes decode the stored full-resolution codestream only.
        if reqs.iter().any(|req| req.level.get() != 0) {
            return None;
        }
        Some((|| {
            let jobs = reqs
                .iter()
                .map(|req| {
                    if let Some(control) = control {
                        control.check_cancelled()?;
                    }
                    self.validate_request(req)?;
                    Ok(crate::decode::jp2k::Jp2kDecodeJob {
                        data: Cow::Borrowed(self.data.as_slice()),
                        expected_width: self.width,
                        expected_height: self.height,
                        rgb_color_space: true,
                        backend: j2k_core::BackendRequest::Cpu,
                    })
                })
                .collect::<Result<Vec<_>, WsiError>>()?;
            crate::decode::jp2k::PreparedJp2kBatch::new(&jobs, workers)
                .map(|prepared| prepared.with_sequential_cpu_images())
        })())
    }

    fn tile_encoded_upper_bound(&self, _req: &TileRequest) -> Result<u64, WsiError> {
        Ok(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        Ok(if reqs.is_empty() {
            0
        } else {
            u64::try_from(self.data.len()).unwrap_or(u64::MAX)
        })
    }

    fn display_tile_encoded_upper_bound(&self, _req: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
    }

    fn associated_encoded_upper_bound(&self, _name: &str) -> Result<u64, WsiError> {
        Ok(0)
    }

    fn region_fastpath_encoded_upper_bound(&self, _req: &RegionRequest) -> Result<u64, WsiError> {
        Ok(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
    }
}
