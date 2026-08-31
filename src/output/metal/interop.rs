use super::{MetalDeviceStorage, MetalDeviceTile};
use crate::{error::WsiError, PixelFormat};
use j2k_core::DeviceSubmission;
use j2k_metal_support::{MetalImageLayout, ResidentMetalImage, SubmittedMetalImages};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandEncoder,
    MTLComputeCommandEncoder, MTLDevice, MTLResource,
};

use super::MetalBuffer;

type CommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;

// SAFETY: the converter owns retained Metal queue, library, and immutable
// pipeline objects, all documented by Metal as cross-thread resources. Lazy
// pipeline initialization is serialized by `OnceLock`.
unsafe impl Send for super::ycbcr::YcbcrToRgb8Converter {}
// SAFETY: shared access exposes immutable handles and creates an independent
// command buffer per conversion; no unsynchronized CPU mutation is reachable.
unsafe impl Sync for super::ycbcr::YcbcrToRgb8Converter {}

pub(super) fn support_error(
    context: &'static str,
    source: j2k_metal_support::MetalSupportError,
) -> WsiError {
    WsiError::Codec {
        codec: context,
        source: Box::new(source),
    }
}

pub(super) fn bind_resident_compute_input(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    index: u64,
    image: &ResidentMetalImage,
) {
    // SAFETY: the binding index is part of the fixed shader ABI, the offset
    // was validated by `ResidentMetalImage`, and support-created command
    // buffers retain the immutable input through completion.
    unsafe {
        encoder.setBuffer_offset_atIndex(
            Some(image.raw_buffer()),
            image.byte_offset(),
            usize::try_from(index).expect("Metal buffer index fits usize"),
        )
    };
}

pub(super) fn bind_compute_buffer(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    index: usize,
    buffer: &ProtocolObject<dyn MTLBuffer>,
) {
    assert!(index < 31, "Metal buffer index exceeds the binding table");
    // SAFETY: every call site uses this allocation according to its fixed
    // shader ABI, the offset is zero, the index was validated, and the
    // support-created command buffer retains bound resources to completion.
    unsafe { encoder.setBuffer_offset_atIndex(Some(buffer), 0, index) };
}

pub(super) fn bind_ycbcr_params(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    index: usize,
    params: &super::ycbcr::YcbcrToRgb8Params,
) {
    assert!(index < 31, "Metal byte index exceeds the binding table");
    let pointer = std::ptr::NonNull::from(params).cast();
    // SAFETY: `YcbcrToRgb8Params` is `repr(C)` with four initialized `u32`
    // fields and no padding. Metal copies these bytes during this call, and
    // the fixed shader ABI uses the same layout and binding index.
    unsafe {
        encoder.setBytes_length_atIndex(
            pointer,
            core::mem::size_of::<super::ycbcr::YcbcrToRgb8Params>(),
            index,
        )
    };
}

#[cfg(test)]
pub(super) fn bind_probe_coordinate(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    index: usize,
    coordinate: &[u32; 2],
) {
    assert!(index < 31, "Metal byte index exceeds the binding table");
    let pointer = std::ptr::NonNull::from(coordinate).cast();
    // SAFETY: the two initialized `u32` values exactly match the probe
    // shader's `uint2` value binding and Metal copies them synchronously.
    unsafe { encoder.setBytes_length_atIndex(pointer, core::mem::size_of_val(coordinate), index) };
}

pub(super) fn submit_ycbcr_images(
    device: &ProtocolObject<dyn MTLDevice>,
    command_buffer: CommandBuffer,
    outputs: Vec<(MetalBuffer, MetalImageLayout)>,
    inputs: Vec<ResidentMetalImage>,
) -> Result<SubmittedMetalImages, WsiError> {
    // SAFETY: the YCbCr converter passes fresh destination allocations, its
    // command buffer is their sole writer, and every bound resident input is
    // retained in `inputs` until completion.
    unsafe { SubmittedMetalImages::from_uncommitted(device, command_buffer, outputs, inputs) }
        .map_err(|source| support_error("metal-ycbcr", source))
}

pub(super) fn download_resident_rows(
    image: &ResidentMetalImage,
    row_bytes: usize,
    byte_len: usize,
) -> Result<Vec<u8>, WsiError> {
    let raw_input = unsafe {
        // SAFETY: the immutable resident image is borrowed for the complete
        // synchronous blit and retained by the submission until completion.
        image.raw_buffer()
    };
    let device = raw_input.device();
    let queue = j2k_metal_support::checked_command_queue(&device)
        .map_err(|source| support_error("metal-download-queue", source))?;
    let command_buffer = j2k_metal_support::checked_command_buffer(&queue)
        .map_err(|source| support_error("metal-download-command", source))?;
    let output = j2k_metal_support::checked_shared_buffer(&device, byte_len)
        .map_err(|source| support_error("metal-download-allocation", source))?;
    let blit = j2k_metal_support::checked_blit_command_encoder(&command_buffer)
        .map_err(|source| support_error("metal-download-blit", source))?;

    for row in 0..usize::try_from(image.dimensions().1).unwrap_or(usize::MAX) {
        let source_offset = row
            .checked_mul(image.pitch_bytes())
            .and_then(|offset| image.byte_offset().checked_add(offset))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Metal download source offset overflow".into())
            })?;
        let destination_offset = row.checked_mul(row_bytes).ok_or_else(|| {
            WsiError::DisplayConversion("Metal download destination offset overflow".into())
        })?;
        // SAFETY: ResidentMetalImage validated the source range, the tight
        // layout validated the destination range, and the fresh output buffer
        // has no other reader or writer before command completion.
        unsafe {
            blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                raw_input,
                source_offset,
                &output,
                destination_offset,
                row_bytes,
            );
        }
    }
    blit.endEncoding();

    let output_layout =
        MetalImageLayout::new(0, image.dimensions(), row_bytes, image.pixel_format())
            .map_err(|source| support_error("metal-download-layout", source))?;
    // SAFETY: the fresh output has exactly one pending writer in this command
    // buffer. The immutable input is retained until the blit completes.
    let submitted = unsafe {
        SubmittedMetalImages::from_uncommitted(
            &device,
            command_buffer,
            vec![(output, output_layout)],
            vec![image.clone()],
        )
    }
    .map_err(|source| support_error("metal-download-submit", source))?;
    let mut outputs = submitted
        .wait()
        .map_err(|source| support_error("metal-download-wait", source))?;
    let output = outputs.pop().ok_or_else(|| {
        WsiError::DisplayConversion("Metal download completed without an output image".into())
    })?;
    // SAFETY: the blit has completed, the shared output is immutable, and the
    // checked helper validates the exact tight byte range before copying.
    unsafe {
        j2k_metal_support::checked_buffer_read_vec::<u8>(
            output.raw_buffer(),
            output.byte_offset(),
            byte_len,
        )
    }
    .map_err(|source| support_error("metal-download-read", source))
}

#[cfg(test)]
pub(super) fn resident_test_image(
    device: &ProtocolObject<dyn MTLDevice>,
    bytes: &[u8],
    dimensions: (u32, u32),
    pitch_bytes: usize,
) -> ResidentMetalImage {
    let buffer = j2k_metal_support::checked_shared_buffer_with_slice(device, bytes)
        .expect("test Metal upload");
    let layout = MetalImageLayout::new(0, dimensions, pitch_bytes, j2k_core::PixelFormat::Rgb8)
        .expect("test resident layout");
    // SAFETY: the synchronous upload is complete and the owned buffer has no
    // surviving writable alias.
    unsafe { ResidentMetalImage::from_completed_buffer(buffer, layout) }
        .expect("test resident image")
}

#[cfg(test)]
pub(crate) fn resident_bytes(image: &ResidentMetalImage) -> Vec<u8> {
    // SAFETY: test output is complete and the immutable resident allocation is
    // read only for the duration of this snapshot.
    unsafe {
        j2k_metal_support::checked_buffer_read_vec::<u8>(
            image.raw_buffer(),
            image.byte_offset(),
            image.byte_len(),
        )
    }
    .expect("resident test readback")
}

#[cfg(test)]
pub(super) fn u64_buffer_values(buffer: &ProtocolObject<dyn MTLBuffer>, len: usize) -> Vec<u64> {
    // SAFETY: the test command buffer has completed and the shared output is
    // read only while this snapshot is created.
    unsafe { j2k_metal_support::checked_buffer_read_vec::<u64>(buffer, 0, len) }
        .expect("test u64 Metal readback")
}

impl MetalDeviceTile {
    /// Adopt a completed, uniquely controlled Metal buffer as a device tile.
    ///
    /// # Safety
    ///
    /// All writes to the described range must have completed. The caller must
    /// ensure no surviving raw handle mutates the allocation while the tile or
    /// any clone remains alive.
    pub unsafe fn from_completed_buffer(
        buffer: MetalBuffer,
        byte_offset: usize,
        width: u32,
        height: u32,
        pitch_bytes: usize,
        format: PixelFormat,
    ) -> Result<Self, WsiError> {
        let j2k_format = j2k_core::PixelFormat::from(format);
        let layout = MetalImageLayout::new(byte_offset, (width, height), pitch_bytes, j2k_format)
            .map_err(|source| support_error("metal-tile-layout", source))?;
        // SAFETY: upheld by this method's caller contract.
        let image = unsafe { ResidentMetalImage::from_completed_buffer(buffer, layout) }
            .map_err(|source| support_error("metal-tile-adoption", source))?;
        Self::from_resident(image)
    }

    /// Deprecated alias for completed raw-buffer adoption.
    ///
    /// # Safety
    ///
    /// The contract is identical to [`MetalDeviceTile::from_completed_buffer`].
    #[deprecated(note = "use from_completed_buffer or the safe from_resident constructor")]
    pub unsafe fn from_buffer(
        buffer: MetalBuffer,
        byte_offset: usize,
        width: u32,
        height: u32,
        pitch_bytes: usize,
        format: PixelFormat,
    ) -> Result<Self, WsiError> {
        // SAFETY: forwarded unchanged to the documented adoption boundary.
        unsafe {
            Self::from_completed_buffer(buffer, byte_offset, width, height, pitch_bytes, format)
        }
    }

    /// Borrow the raw Metal allocation for audited downstream interop.
    ///
    /// # Safety
    ///
    /// The resident storage may be bound only for reads whose submission
    /// retains this tile until completion.
    pub unsafe fn raw_buffer(&self) -> (&ProtocolObject<dyn MTLBuffer>, usize) {
        match &self.storage {
            MetalDeviceStorage::Resident { image } => {
                // SAFETY: the caller accepts the resident raw-read contract.
                (unsafe { image.raw_buffer() }, image.byte_offset())
            }
        }
    }
}
