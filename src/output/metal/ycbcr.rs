use crate::{error::WsiError, PixelFormat};
use j2k_core::DeviceSubmission;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{
    MTLBuffer, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder,
    MTLComputePipelineState,
};
use std::sync::OnceLock;

use super::{interop, MetalDeviceTile};

pub(crate) struct YcbcrToRgb8Converter {
    loader: j2k_metal_support::MetalPipelineLoader,
    pipeline_u32: Pipeline,
    pipeline_u64: OnceLock<Result<Pipeline, String>>,
    queue: CommandQueue,
}

type Buffer = Retained<ProtocolObject<dyn MTLBuffer>>;
type CommandQueue = Retained<ProtocolObject<dyn MTLCommandQueue>>;
type Pipeline = Retained<ProtocolObject<dyn MTLComputePipelineState>>;

impl core::fmt::Debug for YcbcrToRgb8Converter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("YcbcrToRgb8Converter")
            .finish_non_exhaustive()
    }
}

impl YcbcrToRgb8Converter {
    pub(super) fn new(session: &j2k_metal::MetalBackendSession) -> Result<Self, WsiError> {
        let loader =
            j2k_metal_support::MetalPipelineLoader::new(session.device(), YCBCR_TO_RGB8_METAL)
                .map_err(|source| interop::support_error("metal-ycbcr-shader", source))?;
        let pipeline_u32 = loader
            .pipeline("wsi_rs_ycbcr8_to_rgb8_u32")
            .map_err(|source| interop::support_error("metal-ycbcr-u32-pipeline", source))?;
        let queue = j2k_metal_support::checked_command_queue(session.device())
            .map_err(|source| interop::support_error("metal-ycbcr-command-queue", source))?;
        Ok(Self {
            loader,
            pipeline_u32,
            pipeline_u64: OnceLock::new(),
            queue,
        })
    }

    pub(super) fn convert_tile(&self, tile: &MetalDeviceTile) -> Result<MetalDeviceTile, WsiError> {
        let mut converted = self.convert_tiles(std::slice::from_ref(tile))?;
        converted.pop().ok_or_else(|| WsiError::Unsupported {
            reason: "Metal YCbCr conversion produced no output tile".into(),
        })
    }

    pub(super) fn convert_tiles(
        &self,
        tiles: &[MetalDeviceTile],
    ) -> Result<Vec<MetalDeviceTile>, WsiError> {
        if tiles.is_empty() {
            return Ok(Vec::new());
        }

        let jobs = tiles
            .iter()
            .map(|tile| self.prepare_job(tile))
            .collect::<Result<Vec<_>, _>>()?;
        let command_buffer = j2k_metal_support::checked_command_buffer(&self.queue)
            .map_err(|source| interop::support_error("metal-ycbcr-command-buffer", source))?;
        for job in &jobs {
            self.encode_job(&command_buffer, job)?;
        }
        let inputs = jobs.iter().map(|job| job.input.clone()).collect();
        let outputs = jobs
            .into_iter()
            .map(|job| (job.dst_buffer, job.output_layout))
            .collect();
        let device = self.queue.device();
        let submitted = interop::submit_ycbcr_images(&device, command_buffer, outputs, inputs)?;
        submitted
            .wait()
            .map_err(|source| interop::support_error("metal-ycbcr-completion", source))?
            .into_iter()
            .map(MetalDeviceTile::from_resident)
            .collect()
    }

    fn prepare_job(&self, tile: &MetalDeviceTile) -> Result<YcbcrToRgb8Job, WsiError> {
        if tile.format != PixelFormat::Rgb8 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "Metal YCbCr conversion requires Rgb8-compatible source planes, got {:?}",
                    tile.format
                ),
            });
        }
        let device = self.queue.device();
        let image = tile.resident_image_for_device(&device)?;
        let address_plan =
            YcbcrAddressPlan::new(tile.width, tile.height, tile.pitch_bytes, image.byte_len())?;
        let row_bytes = address_plan.dst_pitch;
        let dst_len = address_plan.dst_len;
        let dst_buffer = j2k_metal_support::checked_shared_buffer_for_len::<u8>(&device, dst_len)
            .map_err(|source| {
            interop::support_error("metal-ycbcr-output-allocation", source)
        })?;
        let output_layout = j2k_metal_support::MetalImageLayout::new(
            0,
            (tile.width, tile.height),
            row_bytes,
            j2k_core::PixelFormat::Rgb8,
        )
        .map_err(|source| interop::support_error("metal-ycbcr-output-layout", source))?;
        Ok(YcbcrToRgb8Job {
            input: image.clone(),
            dst_buffer,
            output_layout,
            params: YcbcrToRgb8Params {
                width: tile.width,
                height: tile.height,
                src_pitch: address_plan.src_pitch,
                dst_pitch: u32::try_from(address_plan.dst_pitch).map_err(|_| {
                    WsiError::Unsupported {
                        reason: "Metal YCbCr conversion destination pitch exceeds u32".into(),
                    }
                })?,
            },
            address_width: address_plan.address_width,
        })
    }

    fn encode_job(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        job: &YcbcrToRgb8Job,
    ) -> Result<(), WsiError> {
        let encoder = j2k_metal_support::checked_compute_command_encoder(command_buffer)
            .map_err(|source| interop::support_error("metal-ycbcr-command-encoder", source))?;
        let pipeline = match job.address_width {
            YcbcrAddressWidth::U32 => self.pipeline_u32.as_ref(),
            YcbcrAddressWidth::U64 => self.pipeline_u64()?,
        };
        encoder.setComputePipelineState(pipeline);
        interop::bind_resident_compute_input(&encoder, 0, &job.input);
        interop::bind_compute_buffer(&encoder, 1, &job.dst_buffer);
        interop::bind_ycbcr_params(&encoder, 2, &job.params);
        j2k_metal_support::dispatch_2d_pipeline(
            &encoder,
            pipeline,
            (job.params.width, job.params.height),
        );
        encoder.endEncoding();
        Ok(())
    }

    pub(super) fn pipeline_u64(
        &self,
    ) -> Result<&ProtocolObject<dyn MTLComputePipelineState>, WsiError> {
        self.pipeline_u64
            .get_or_init(|| {
                self.loader
                    .pipeline("wsi_rs_ycbcr8_to_rgb8")
                    .map_err(|source| {
                        format!("Metal u64 YCbCr conversion pipeline unavailable: {source}")
                    })
            })
            .as_deref()
            .map_err(|message| WsiError::Codec {
                codec: "j2k",
                source: Box::new(WsiError::Jp2k(message.clone())),
            })
    }
}

struct YcbcrToRgb8Job {
    input: j2k_metal_support::ResidentMetalImage,
    dst_buffer: Buffer,
    output_layout: j2k_metal_support::MetalImageLayout,
    params: YcbcrToRgb8Params,
    address_width: YcbcrAddressWidth,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum YcbcrAddressWidth {
    U32,
    U64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct YcbcrAddressPlan {
    pub(super) src_pitch: u32,
    pub(super) dst_pitch: usize,
    pub(super) dst_len: usize,
    pub(super) address_width: YcbcrAddressWidth,
}

impl YcbcrAddressPlan {
    pub(super) fn new(
        width: u32,
        height: u32,
        src_pitch: usize,
        src_len: usize,
    ) -> Result<Self, WsiError> {
        if width == 0 || height == 0 {
            return Err(WsiError::Unsupported {
                reason: "Metal YCbCr conversion requires nonzero dimensions".into(),
            });
        }
        let bytes_per_pixel = PixelFormat::Rgb8.bytes_per_pixel();
        let dst_pitch = (width as usize)
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| WsiError::Unsupported {
                reason: "Metal YCbCr conversion row byte count overflow".into(),
            })?;
        if src_pitch < dst_pitch {
            return Err(WsiError::Unsupported {
                reason: "Metal YCbCr conversion source pitch is shorter than row bytes".into(),
            });
        }
        let src_pitch_u32 = u32::try_from(src_pitch).map_err(|_| WsiError::Unsupported {
            reason: "Metal YCbCr conversion source pitch exceeds the shader ABI".into(),
        })?;
        let dst_len =
            dst_pitch
                .checked_mul(height as usize)
                .ok_or_else(|| WsiError::Unsupported {
                    reason: "Metal YCbCr conversion output byte count overflow".into(),
                })?;
        let max_src_byte = Self::max_byte(width, height, src_pitch_u32);
        // `src_pitch` was converted to u32 above and is at least `dst_pitch`.
        let dst_pitch_u32 = u32::try_from(dst_pitch).expect("validated destination pitch");
        let max_dst_byte = Self::max_byte(width, height, dst_pitch_u32);
        let required_src = max_src_byte + 1;
        if required_src > src_len as u64 {
            return Err(WsiError::Unsupported {
                reason: "Metal YCbCr conversion source span exceeds the resident image".into(),
            });
        }
        let required_dst = max_dst_byte + 1;
        if required_dst > dst_len as u64 {
            return Err(WsiError::Unsupported {
                reason: "Metal YCbCr conversion destination span exceeds its allocation".into(),
            });
        }
        let address_width =
            if max_src_byte <= u64::from(u32::MAX) && max_dst_byte <= u64::from(u32::MAX) {
                YcbcrAddressWidth::U32
            } else {
                YcbcrAddressWidth::U64
            };
        Ok(Self {
            src_pitch: src_pitch_u32,
            dst_pitch,
            dst_len,
            address_width,
        })
    }

    pub(super) fn max_byte(width: u32, height: u32, pitch: u32) -> u64 {
        // With u32 dimensions and pitch, the maximum expression is u64::MAX - 1.
        u64::from(height - 1) * u64::from(pitch) + u64::from(width - 1) * 3 + 2
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct YcbcrToRgb8Params {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) src_pitch: u32,
    pub(super) dst_pitch: u32,
}

pub(super) const YCBCR_TO_RGB8_METAL: &str = include_str!("ycbcr.metal");
