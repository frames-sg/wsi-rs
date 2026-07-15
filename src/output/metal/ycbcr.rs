use crate::{error::WsiError, PixelFormat};
use j2k_core::DeviceSubmission;
use std::sync::OnceLock;

use super::{interop, MetalDeviceTile};

pub(crate) struct YcbcrToRgb8Converter {
    library: metal::Library,
    pipeline_u32: metal::ComputePipelineState,
    pipeline_u64: OnceLock<Result<metal::ComputePipelineState, String>>,
    queue: metal::CommandQueue,
}

impl core::fmt::Debug for YcbcrToRgb8Converter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("YcbcrToRgb8Converter")
            .finish_non_exhaustive()
    }
}

impl YcbcrToRgb8Converter {
    pub(super) fn new(session: &j2k_metal::MetalBackendSession) -> Result<Self, WsiError> {
        let options = metal::CompileOptions::new();
        let library = session
            .device()
            .new_library_with_source(YCBCR_TO_RGB8_METAL, &options)
            .map_err(|message| WsiError::Codec {
                codec: "j2k",
                source: Box::new(WsiError::Jp2k(format!(
                    "Metal YCbCr conversion shader failed to compile: {message}"
                ))),
            })?;
        let function = library
            .get_function("wsi_rs_ycbcr8_to_rgb8_u32", None)
            .map_err(|message| WsiError::Codec {
                codec: "j2k",
                source: Box::new(WsiError::Jp2k(format!(
                    "Metal u32 YCbCr conversion function unavailable: {message}"
                ))),
            })?;
        let pipeline_u32 = session
            .device()
            .new_compute_pipeline_state_with_function(&function)
            .map_err(|message| WsiError::Codec {
                codec: "j2k",
                source: Box::new(WsiError::Jp2k(format!(
                    "Metal u32 YCbCr conversion pipeline unavailable: {message}"
                ))),
            })?;
        let queue = session.device().new_command_queue();
        Ok(Self {
            library,
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
        let submitted =
            interop::submit_ycbcr_images(self.queue.device(), command_buffer, outputs, inputs)?;
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
        let image = tile.resident_image_for_device(self.queue.device())?;
        let address_plan =
            YcbcrAddressPlan::new(tile.width, tile.height, tile.pitch_bytes, image.byte_len())?;
        let row_bytes = address_plan.dst_pitch;
        let dst_len = address_plan.dst_len;
        let dst_buffer =
            j2k_metal_support::checked_shared_buffer_for_len::<u8>(self.queue.device(), dst_len)
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
        command_buffer: &metal::CommandBufferRef,
        job: &YcbcrToRgb8Job,
    ) -> Result<(), WsiError> {
        let encoder = command_buffer.new_compute_command_encoder();
        let pipeline = match job.address_width {
            YcbcrAddressWidth::U32 => self.pipeline_u32.as_ref(),
            YcbcrAddressWidth::U64 => self.pipeline_u64()?,
        };
        encoder.set_compute_pipeline_state(pipeline);
        interop::bind_resident_compute_input(encoder, 0, &job.input);
        encoder.set_buffer(1, Some(&job.dst_buffer), 0);
        encoder.set_bytes(
            2,
            core::mem::size_of::<YcbcrToRgb8Params>() as u64,
            std::ptr::from_ref(&job.params).cast(),
        );
        let width = pipeline.thread_execution_width().max(1);
        let max_threads = pipeline.max_total_threads_per_threadgroup().max(width);
        let height = (max_threads / width).max(1);
        encoder.dispatch_threads(
            metal::MTLSize {
                width: u64::from(job.params.width),
                height: u64::from(job.params.height),
                depth: 1,
            },
            metal::MTLSize {
                width,
                height,
                depth: 1,
            },
        );
        encoder.end_encoding();
        Ok(())
    }

    fn pipeline_u64(&self) -> Result<&metal::ComputePipelineStateRef, WsiError> {
        self.pipeline_u64
            .get_or_init(|| {
                let function = self
                    .library
                    .get_function("wsi_rs_ycbcr8_to_rgb8", None)
                    .map_err(|message| {
                        format!("Metal u64 YCbCr conversion function unavailable: {message}")
                    })?;
                self.queue
                    .device()
                    .new_compute_pipeline_state_with_function(&function)
                    .map_err(|message| {
                        format!("Metal u64 YCbCr conversion pipeline unavailable: {message}")
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
    dst_buffer: metal::Buffer,
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
        let max_src_byte = Self::max_byte(width, height, src_pitch_u32)?;
        let dst_pitch_u32 = u32::try_from(dst_pitch).map_err(|_| WsiError::Unsupported {
            reason: "Metal YCbCr conversion destination pitch exceeds the shader ABI".into(),
        })?;
        let max_dst_byte = Self::max_byte(width, height, dst_pitch_u32)?;
        let required_src = max_src_byte
            .checked_add(1)
            .ok_or_else(|| WsiError::Unsupported {
                reason: "Metal YCbCr conversion source span overflows u64".into(),
            })?;
        if required_src > src_len as u64 {
            return Err(WsiError::Unsupported {
                reason: "Metal YCbCr conversion source span exceeds the resident image".into(),
            });
        }
        let required_dst = max_dst_byte
            .checked_add(1)
            .ok_or_else(|| WsiError::Unsupported {
                reason: "Metal YCbCr conversion destination span overflows u64".into(),
            })?;
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

    pub(super) fn max_byte(width: u32, height: u32, pitch: u32) -> Result<u64, WsiError> {
        u64::from(height - 1)
            .checked_mul(u64::from(pitch))
            .and_then(|row| row.checked_add(u64::from(width - 1).checked_mul(3)?))
            .and_then(|first_channel| first_channel.checked_add(2))
            .ok_or_else(|| WsiError::Unsupported {
                reason: "Metal YCbCr conversion address calculation overflow".into(),
            })
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
