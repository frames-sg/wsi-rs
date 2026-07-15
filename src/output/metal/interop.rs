use super::{MetalDeviceStorage, MetalDeviceTile};
use crate::{error::WsiError, PixelFormat};
use j2k_metal_support::{MetalImageLayout, ResidentMetalImage, SubmittedMetalImages};
use metal::{Buffer, CommandBuffer, ComputeCommandEncoderRef, DeviceRef};

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
    encoder: &ComputeCommandEncoderRef,
    index: u64,
    image: &ResidentMetalImage,
) {
    // SAFETY: this audited operation binds the logically immutable resident
    // allocation for a GPU read. The submission owner separately retains the
    // image through completion.
    encoder.set_buffer(
        index,
        Some(unsafe { image.raw_buffer() }),
        image.byte_offset() as u64,
    );
}

pub(super) fn submit_ycbcr_images(
    device: &DeviceRef,
    command_buffer: CommandBuffer,
    outputs: Vec<(Buffer, MetalImageLayout)>,
    inputs: Vec<ResidentMetalImage>,
) -> Result<SubmittedMetalImages, WsiError> {
    // SAFETY: the YCbCr converter passes fresh destination allocations, its
    // command buffer is their sole writer, and every bound resident input is
    // retained in `inputs` until completion.
    unsafe { SubmittedMetalImages::from_uncommitted(device, command_buffer, outputs, inputs) }
        .map_err(|source| support_error("metal-ycbcr", source))
}

#[cfg(test)]
pub(super) fn resident_test_image(
    device: &DeviceRef,
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
pub(super) fn resident_bytes(image: &ResidentMetalImage) -> Vec<u8> {
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
pub(super) fn u64_buffer_values(buffer: &Buffer, len: usize) -> Vec<u64> {
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
        buffer: Buffer,
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
        buffer: Buffer,
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
    /// Resident storage may be bound only for reads whose submission retains
    /// this tile until completion. Legacy buffer storage remains untrusted and
    /// requires the caller to establish all synchronization and aliasing rules.
    #[allow(deprecated)]
    pub unsafe fn raw_buffer(&self) -> (&Buffer, usize) {
        match &self.storage {
            MetalDeviceStorage::Resident { image } => {
                // SAFETY: the caller accepts the resident raw-read contract.
                (unsafe { image.raw_buffer() }, image.byte_offset())
            }
            MetalDeviceStorage::Buffer {
                buffer,
                byte_offset,
            } => (buffer, *byte_offset),
        }
    }
}
