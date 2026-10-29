#[cfg(any(feature = "metal", feature = "cuda"))]
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::limits::{read_to_end_bounded, MAX_COMPRESSED_INPUT_BYTES};
use crate::core::registry::{
    BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe, DatasetReader, FormatProbe,
    ManagedSlideReader, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::{
    AssociatedImage, AxesShape, ByteRange, ChannelInfo, Compression, CpuTile, Dataset,
    EncodedCodec, EncodedTilePhotometricInterpretation, EncodedUnit, Level, NotAddressableReason,
    PixelRect, PlaneSelection, RawCompressedTile, RegionRequest, SampleType, Scene, Series,
    StoredFraming, StoredTile, TileCodecKind, TileLayout, TileLocation, TileRequest,
    TileViewRequest,
};
use crate::decode::jp2k::{
    decode_jp2k_region_to_sample_buffer, decode_jp2k_to_sample_buffer,
    jp2k_decodable_reduction_levels, reduced_jp2k_dimensions, Jp2kColorSpace,
};
use crate::decode::jp2k_codestream::{codestream_header_from_view, validate_component_contract};
use crate::error::WsiError;
use crate::properties::Properties;
use j2k::CpuDecodeParallelism;
use j2k_core::Rect;
use rayon::prelude::*;

#[cfg(test)]
mod tests;

const MARKER_SOC_BYTES: [u8; 2] = [0xFF, 0x4F];
/// Probe vendor and `openslide.vendor`; also salts the dataset quickhash.
const VENDOR: &str = "raw-jp2k";

/// How the reader divides levels into tiles.
#[derive(Debug, Clone, Copy)]
struct LevelGrid {
    /// A level with no side longer than this decodes whole, as one tile.
    whole_level_max_side: u32,
    /// Tile side for larger levels, each tile a region decode.
    tile_side: u32,
}

impl LevelGrid {
    /// On an 18000x12000 codestream, a whole 2250x1500 level decodes faster
    /// than its tiles and a whole 4500x3000 level no faster; whole levels from
    /// 9000x6000 up exceed J2K's decoder allocation cap. A viewport's 1024 px
    /// tiles decode in one parallel round.
    const DEFAULT: Self = Self {
        whole_level_max_side: 4096,
        tile_side: 1024,
    };

    fn tile_layout(self, (width, height): (u32, u32)) -> TileLayout {
        if width.max(height) <= self.whole_level_max_side {
            return TileLayout::Regular {
                tile_width: width,
                tile_height: height,
                tiles_across: 1,
                tiles_down: 1,
            };
        }
        TileLayout::Regular {
            tile_width: self.tile_side,
            tile_height: self.tile_side,
            tiles_across: u64::from(width.div_ceil(self.tile_side)),
            tiles_down: u64::from(height.div_ceil(self.tile_side)),
        }
    }
}

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
        Ok(Box::new(open_raw_jp2k(path, config, LevelGrid::DEFAULT)?))
    }
}

fn open_raw_jp2k(
    path: &Path,
    config: BackendOpenConfig,
    grid: LevelGrid,
) -> Result<RawJp2kReader, WsiError> {
    let file = File::open(path).map_err(|source| WsiError::io_with_path(path, source))?;
    let max_input = MAX_COMPRESSED_INPUT_BYTES.min(config.limits.encoded_unit_bytes());
    let declared_len = file
        .metadata()
        .map_err(|source| WsiError::io_with_path(path, source))?
        .len();
    if declared_len > max_input {
        return Err(WsiError::ResourceLimit {
            resource: "encoded tile/frame unit",
            requested: declared_len,
            limit: max_input,
        });
    }
    let data = read_to_end_bounded(file, max_input, "raw JP2K input")
        .map_err(|source| WsiError::io_with_path(path, source))?;
    let view = j2k::J2kView::parse(&data).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    if let Some(layout) = &view.info().tile_layout {
        // Every tile needs at least one 12-byte SOT segment and a 2-byte
        // SOD marker. Bound the declared grid before reduction probing or
        // decoding can allocate and process metadata for absent tiles.
        let tile_count = u64::from(layout.tiles_x) * u64::from(layout.tiles_y);
        if tile_count > data.len() as u64 / 14 {
            return Err(WsiError::Jp2k(
                "raw JP2K input is too short for its declared tile grid".into(),
            ));
        }
    }
    let header = codestream_header_from_view(&view)?;
    // Each tile's decode bounds its own output, so the whole image may exceed
    // the decoded-image limit.
    validate_component_contract(&header)?;
    let dimensions = (header.image_width, header.image_height);
    Ok(RawJp2kReader {
        dataset: dataset_for_codestream(
            path,
            &data,
            dimensions,
            jp2k_decodable_reduction_levels(&data)?,
            grid,
        )?,
        data,
        path: Arc::from(path),
        width: header.image_width,
        height: header.image_height,
    })
}

/// Each decodable wavelet reduction is exposed as a pyramid level. Reduced
/// levels are decoded from the codestream's lower resolutions on demand. Levels
/// larger than `grid` allows are tiled, each tile a region decode, so a read
/// decodes only the tiles it covers.
fn dataset_for_codestream(
    path: &Path,
    data: &[u8],
    dimensions: (u32, u32),
    reduction_levels: u8,
    grid: LevelGrid,
) -> Result<Dataset, WsiError> {
    let levels = (0..=reduction_levels)
        .map(|reduction| {
            let (width, height) = reduced_jp2k_dimensions(dimensions, reduction)?;
            Ok(Level {
                dimensions: (u64::from(width), u64::from(height)),
                // Reduced pixels cover exact 2^k source blocks, so this is not
                // the rounded width ratio.
                downsample: f64::from(1_u32 << reduction),
                tile_layout: grid.tile_layout((width, height)),
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

fn is_one_tile(level: &Level) -> bool {
    matches!(
        level.tile_layout,
        TileLayout::Regular {
            tiles_across: 1,
            tiles_down: 1,
            ..
        }
    )
}

struct RawJp2kReader {
    dataset: Dataset,
    /// The whole file, which is the codestream.
    data: Vec<u8>,
    /// The file as opened, shared by its tile location.
    path: Arc<Path>,
    width: u32,
    height: u32,
}

/// One tile's decode: a codestream region, in full-resolution coordinates, at
/// a reduction.
#[derive(Debug, Clone, Copy)]
struct TileDecode {
    reduction: u8,
    region: Rect,
    /// The tile is its whole level.
    whole_level: bool,
}

impl RawJp2kReader {
    fn level_count(&self) -> usize {
        self.dataset.scenes[0].series[0].levels.len()
    }

    fn levels(&self) -> &[Level] {
        &self.dataset.scenes[0].series[0].levels
    }

    fn tile_decode(&self, req: &TileRequest) -> Result<TileDecode, WsiError> {
        self.validate_request(req)?;
        let level = &self.levels()[req.level.get() as usize];
        let reduction = u8::try_from(req.level.get()).map_err(|_| {
            WsiError::tile_read(
                req,
                "raw JP2K reduction level exceeds the codestream ladder",
            )
        })?;
        let tile = level
            .regular_tile(req)?
            .ok_or_else(|| WsiError::tile_read(req, "raw JP2K levels are regular grids"))?;
        // A level pixel covers 2^reduction source pixels; the last row and
        // column of tiles end at the image edge.
        let to_source = |start: u64, len: u32, full: u32| {
            let end = (start + u64::from(len)) << reduction;
            let start = start << reduction;
            Some((
                u32::try_from(start).ok()?,
                u32::try_from(end.min(u64::from(full)) - start).ok()?,
            ))
        };
        let ((x, w), (y, h)) = to_source(tile.x, tile.width, self.width)
            .zip(to_source(tile.y, tile.height, self.height))
            .ok_or_else(|| WsiError::tile_read(req, "raw JP2K tile lies outside the image"))?;
        Ok(TileDecode {
            reduction,
            region: Rect { x, y, w, h },
            whole_level: is_one_tile(level),
        })
    }

    fn decode_tile(
        &self,
        tile: TileDecode,
        parallelism: CpuDecodeParallelism,
    ) -> Result<CpuTile, WsiError> {
        if tile.whole_level && tile.reduction == 0 {
            return decode_jp2k_to_sample_buffer(
                &self.data,
                self.width,
                self.height,
                Jp2kColorSpace::Rgb,
            );
        }
        decode_jp2k_region_to_sample_buffer(
            &self.data,
            tile.region,
            tile.reduction,
            parallelism,
            Jp2kColorSpace::Rgb,
        )
    }

    fn validate_request(&self, req: &TileRequest) -> Result<(), WsiError> {
        if req.scene.get() != 0
            || req.series.get() != 0
            || req.level.get() as usize >= self.level_count()
        {
            return Err(WsiError::tile_read(
                req,
                format!(
                    "raw JP2K source has one scene, one series, and {} levels",
                    self.level_count()
                ),
            ));
        }
        if req.plane.get() != PlaneSelection::default() {
            return Err(WsiError::Unsupported {
                reason: "raw JP2K source has only the default plane".into(),
            });
        }
        Ok(())
    }

    /// Only full resolution is stored as a codestream, and only a full
    /// resolution that is one tile is that codestream. Reduced levels and the
    /// tiles of a larger image are decoded from it on the CPU.
    #[cfg(feature = "cuda")]
    fn validate_full_resolution_request(
        &self,
        req: &TileRequest,
        operation: &str,
    ) -> Result<(), WsiError> {
        let tile = self.tile_decode(req)?;
        if tile.reduction != 0 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw JP2K {operation} is available at full resolution only; reduced levels are decoded on the CPU"
                ),
            });
        }
        if !tile.whole_level {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw JP2K {operation} needs the full resolution as one tile; its tiles are region decodes on the CPU"
                ),
            });
        }
        Ok(())
    }

    /// One decode job for the stored full-resolution codestream.
    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn jp2k_job(
        &self,
        backend: j2k_core::BackendRequest,
    ) -> crate::decode::jp2k::Jp2kDecodeJob<'_> {
        crate::decode::jp2k::Jp2kDecodeJob {
            data: Cow::Borrowed(self.data.as_slice()),
            expected_width: self.width,
            expected_height: self.height,
            rgb_color_space: true,
            backend,
        }
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
        // One tile alone decodes fastest with the codec's own parallelism.
        self.decode_tile(self.tile_decode(req)?, CpuDecodeParallelism::Auto)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.len() <= 1 {
            return reqs.iter().map(|req| self.read_tile_cpu(req)).collect();
        }
        let tiles = reqs
            .iter()
            .map(|req| self.tile_decode(req))
            .collect::<Result<Vec<_>, WsiError>>()?;
        // Grid tiles decode serially side by side, so J2K streams each tile's
        // Tier-1 in bands; a whole level keeps the codec's own parallelism.
        crate::core::batch::install_codec_cpu(|| {
            tiles
                .par_iter()
                .map(|&tile| {
                    let parallelism = if tile.whole_level {
                        CpuDecodeParallelism::Auto
                    } else {
                        CpuDecodeParallelism::Serial
                    };
                    self.decode_tile(tile, parallelism)
                })
                .collect()
        })
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        // Every tile is a region of the one codestream, so a batch decodes as
        // one Metal batch sharing its bytes.
        let regions = reqs
            .iter()
            .map(|req| {
                let tile = self.tile_decode(req)?;
                if crate::decode::jp2k::device_downscale(tile.reduction).is_none() {
                    return Err(WsiError::Unsupported {
                        reason: "raw JP2K Metal decode reduces at most 3 levels; deeper levels are decoded on the CPU".into(),
                    });
                }
                Ok((tile.region, tile.reduction))
            })
            .collect::<Result<Vec<_>, WsiError>>()?;
        crate::decode::jp2k::map_tile_results(
            crate::decode::jp2k::decode_region_batch_jp2k_metal(&self.data, &regions, sessions),
            reqs,
        )
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
            .map(|_| self.jp2k_job(j2k_core::BackendRequest::Cuda))
            .collect::<Vec<_>>();
        crate::decode::jp2k::decode_batch_jp2k_cuda(&jobs, sessions)
            .into_iter()
            .collect()
    }

    /// The file's codestream, unchanged. A full-resolution tile of a larger
    /// image is a window of that one codestream, so it comes back whole with
    /// [`RawCompressedTile::tile_region`] locating the tile. Reduced levels
    /// are decoded from the codestream's lower resolutions and have no
    /// stored bytes of their own.
    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let tile = self.tile_decode(req)?;
        if tile.reduction != 0 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw JP2K level {} is decoded from the codestream's lower resolutions; no stored bytes hold it on their own (tile_location reports NotAddressable(CodestreamWindow))",
                    req.level.get()
                ),
            });
        }
        let mut builder = RawCompressedTile::builder(Compression::Jp2kRgb)
            .dimensions(self.width, self.height)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb);
        if !tile.whole_level {
            let Rect { x, y, w, h } = tile.region;
            builder = builder.tile_region(PixelRect::new(x, y, w, h));
        }
        Ok(builder.data(self.data.clone()).build()?)
    }

    /// The whole file when full resolution is one tile. Other tiles are
    /// windows or reductions of the one codestream, which has no byte range
    /// per wsi-rs tile.
    fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let tile = self.tile_decode(req)?;
        if tile.reduction != 0 || !tile.whole_level {
            return Ok(TileLocation::NotAddressable(
                NotAddressableReason::CodestreamWindow,
            ));
        }
        let file_len = u64::try_from(self.data.len()).unwrap_or(u64::MAX);
        let codec = EncodedCodec::new(
            Compression::Jp2kRgb,
            8,
            3,
            EncodedTilePhotometricInterpretation::Rgb,
        );
        let unit = EncodedUnit::builder(self.path.clone(), codec, self.width, self.height)
            .segment(ByteRange::new(0, file_len))
            .framing(StoredFraming::Standalone)
            .build(file_len)?;
        Ok(TileLocation::Stored(StoredTile::new(
            self.width,
            self.height,
            vec![unit],
        )?))
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
        // Device routes decode the stored full-resolution codestream only, as
        // one tile.
        if !is_one_tile(&self.levels()[0]) || reqs.iter().any(|req| req.level.get() != 0) {
            return None;
        }
        Some((|| {
            let jobs = reqs
                .iter()
                .map(|req| {
                    crate::core::read_control::check_cancelled_opt(control)?;
                    self.tile_decode(req)?;
                    Ok(self.jp2k_job(j2k_core::BackendRequest::Cpu))
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
