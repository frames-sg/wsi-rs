use super::{MetalDeviceStorage, MetalDeviceTile};
use crate::{error::WsiError, PixelFormat};
use j2k_core::DeviceSubmission;
use j2k_metal_support::{MetalImageLayout, ResidentMetalImage, SubmittedMetalImages};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLComputeCommandEncoder, MTLDevice, MTLResource, MTLStorageMode,
};
use std::sync::{Arc, Mutex, OnceLock};

use super::MetalBuffer;

type CommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;

#[derive(Debug)]
struct ReadbackQueue {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    device_id: u64,
}

// SAFETY: Metal command queues are retained cross-thread resources. This
// wrapper exposes no mutation; every transfer creates a separate command buffer.
unsafe impl Send for ReadbackQueue {}
// SAFETY: concurrent users only submit independent command buffers through the
// thread-safe Metal queue and retain immutable resident input owners.
unsafe impl Sync for ReadbackQueue {}

#[derive(Debug, Default)]
pub(super) struct ReadbackQueueCache {
    queue: Mutex<Option<Arc<ReadbackQueue>>>,
}

impl ReadbackQueueCache {
    fn get(&self, device: &ProtocolObject<dyn MTLDevice>) -> Result<Arc<ReadbackQueue>, WsiError> {
        let mut cached = self.queue.lock().unwrap_or_else(|error| error.into_inner());
        let device_id = device.registryID();
        if let Some(queue) = cached.as_ref().filter(|queue| queue.device_id == device_id) {
            return Ok(queue.clone());
        }
        let queue = Arc::new(ReadbackQueue {
            queue: j2k_metal_support::checked_command_queue(device)
                .map_err(|source| support_error("metal-download-queue", source))?,
            device_id,
        });
        crate::core::execution_telemetry::record(
            crate::core::execution_telemetry::Event::ReadbackQueueCreations,
            1,
        );
        *cached = Some(queue.clone());
        Ok(queue)
    }
}

pub(super) struct ReadbackRows<'a> {
    pub(super) image: &'a ResidentMetalImage,
    pub(super) row_bytes: usize,
    pub(super) byte_len: usize,
}

#[cfg(test)]
thread_local! {
    pub(super) static READBACK_STAGING_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
    queue_slot: &OnceLock<Arc<ReadbackQueueCache>>,
) -> Result<Vec<u8>, WsiError> {
    // SAFETY: the completed resident owner guarantees immutable storage for
    // this synchronous read; only its storage mode and initialized rows are read.
    let raw = unsafe { image.raw_buffer() };
    if raw.storageMode() == MTLStorageMode::Shared {
        crate::core::execution_telemetry::record(
            crate::core::execution_telemetry::Event::ReadbackBytes,
            byte_len,
        );
        return copy_completed_shared_rows(image, raw, row_bytes, byte_len);
    }
    let queue = queue_slot.get_or_init(|| Arc::new(ReadbackQueueCache::default()));
    crate::core::batch::exactly_one(
        download_resident_batch(
            &[ReadbackRows {
                image,
                row_bytes,
                byte_len,
            }],
            queue,
        )?,
        "Metal single-image download",
    )
}

pub(super) fn download_resident_batch(
    rows: &[ReadbackRows<'_>],
    queue: &ReadbackQueueCache,
) -> Result<Vec<Vec<u8>>, WsiError> {
    use crate::core::execution_telemetry::{record, Event};
    let mut outputs: Vec<Option<Vec<u8>>> = (0..rows.len()).map(|_| None).collect();
    let mut staged = Vec::new();
    let mut staged_bytes = 0_usize;
    for (index, row) in rows.iter().enumerate() {
        super::tile::enforce_download_limit(row.byte_len)?;
        record(Event::ReadbackBytes, row.byte_len);
        // SAFETY: the resident owner is retained by the caller throughout all
        // copies. No CPU/GPU writer can alias this completed input.
        let raw = unsafe { row.image.raw_buffer() };
        if raw.storageMode() == MTLStorageMode::Shared {
            outputs[index] = Some(copy_completed_shared_rows(
                row.image,
                raw,
                row.row_bytes,
                row.byte_len,
            )?);
            continue;
        }
        if staged_bytes.saturating_add(row.byte_len)
            > super::tile::MAX_DEVICE_DOWNLOAD_BYTES as usize
        {
            download_staged_rows(rows, &staged, staged_bytes, queue, &mut outputs)?;
            staged.clear();
            staged_bytes = 0;
        }
        staged.push((index, staged_bytes));
        staged_bytes = staged_bytes
            .checked_add(row.byte_len)
            .ok_or_else(|| WsiError::DisplayConversion("Metal staging size overflow".into()))?;
    }
    if !staged.is_empty() {
        download_staged_rows(rows, &staged, staged_bytes, queue, &mut outputs)?;
    }
    outputs
        .into_iter()
        .map(|output| {
            output.ok_or_else(|| {
                WsiError::DisplayConversion("Metal readback did not produce an output slot".into())
            })
        })
        .collect()
}

fn download_staged_rows(
    rows: &[ReadbackRows<'_>],
    staged: &[(usize, usize)],
    byte_len: usize,
    queue_cache: &ReadbackQueueCache,
    outputs: &mut [Option<Vec<u8>>],
) -> Result<(), WsiError> {
    use crate::core::execution_telemetry::{record, Event};
    // SAFETY: only the device identity of the immutable first input is read.
    let device = unsafe { rows[staged[0].0].image.raw_buffer().device() };
    for &(index, _) in staged {
        rows[index]
            .image
            .validate_device(&device)
            .map_err(|source| support_error("metal-download-device", source))?;
    }
    let queue = queue_cache.get(&device)?;
    let command = j2k_metal_support::checked_command_buffer(&queue.queue)
        .map_err(|source| support_error("metal-download-command", source))?;
    let buffer = j2k_metal_support::checked_shared_buffer(&device, byte_len)
        .map_err(|source| support_error("metal-download-allocation", source))?;
    let blit = j2k_metal_support::checked_blit_command_encoder(&command)
        .map_err(|source| support_error("metal-download-blit", source))?;
    for &(index, base) in staged {
        encode_readback_rows(&blit, &rows[index], &buffer, base)?;
    }
    blit.endEncoding();
    // The staging image is a byte container. Every byte is initialized by the
    // disjoint tightly packed row spans encoded above, with no padding gaps.
    let width = u32::try_from(byte_len)
        .map_err(|_| WsiError::DisplayConversion("Metal staging width overflow".into()))?;
    let layout = MetalImageLayout::new(0, (width, 1), byte_len, j2k_core::PixelFormat::Gray8)
        .map_err(|source| support_error("metal-download-layout", source))?;
    let inputs = staged
        .iter()
        .map(|&(index, _)| rows[index].image.clone())
        .collect();
    // SAFETY: the staging buffer is fresh and this command is its only writer;
    // the submission retains all completed immutable source images until done.
    let submitted = unsafe {
        SubmittedMetalImages::from_uncommitted(&device, command, vec![(buffer, layout)], inputs)
    }
    .map_err(|source| support_error("metal-download-submit", source))?;
    record(Event::ReadbackSubmissions, 1);
    record(Event::ReadbackStagingBytes, byte_len);
    record(Event::MetalCompletionWaits, 1);
    #[cfg(test)]
    READBACK_STAGING_BYTES.with(|bytes| bytes.set(bytes.get() + byte_len));
    let output = crate::core::batch::exactly_one(
        submitted
            .wait()
            .map_err(|source| support_error("metal-download-wait", source))?,
        "Metal staging completion",
    )?;
    for &(index, offset) in staged {
        // SAFETY: command completion initialized the entire shared staging
        // buffer; the checked helper bounds each disjoint logical tile span.
        outputs[index] = Some(
            unsafe {
                j2k_metal_support::checked_buffer_read_vec::<u8>(
                    output.raw_buffer(),
                    offset,
                    rows[index].byte_len,
                )
            }
            .map_err(|source| support_error("metal-download-read", source))?,
        );
    }
    Ok(())
}

fn encode_readback_rows(
    blit: &ProtocolObject<dyn MTLBlitCommandEncoder>,
    row: &ReadbackRows<'_>,
    output: &ProtocolObject<dyn MTLBuffer>,
    base: usize,
) -> Result<(), WsiError> {
    let image = row.image;
    let height = image.dimensions().1 as usize;
    let span = height
        .checked_sub(1)
        .and_then(|n| n.checked_mul(image.pitch_bytes()))
        .and_then(|n| n.checked_add(row.row_bytes));
    if height == 0
        || row.row_bytes > image.pitch_bytes()
        || height.checked_mul(row.row_bytes) != Some(row.byte_len)
        || span.is_none_or(|span| span > image.byte_len())
        || base
            .checked_add(row.byte_len)
            .is_none_or(|end| end > output.length())
    {
        return Err(WsiError::DisplayConversion(
            "Metal readback rows exceed their source or staging layout".into(),
        ));
    }
    let tight = image.pitch_bytes() == row.row_bytes;
    let copies = if tight { 1 } else { height };
    let bytes = if tight { row.byte_len } else { row.row_bytes };
    for index in 0..copies {
        let source_offset = index
            .checked_mul(image.pitch_bytes())
            .and_then(|n| n.checked_add(image.byte_offset()))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Metal download source offset overflow".into())
            })?;
        let destination = index
            .checked_mul(row.row_bytes)
            .and_then(|n| base.checked_add(n))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Metal download destination offset overflow".into())
            })?;
        // SAFETY: validated resident/source and staging spans cover this copy;
        // the input is immutable and this command exclusively writes output.
        unsafe {
            blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                image.raw_buffer(),
                source_offset,
                output,
                destination,
                bytes,
            );
        }
    }
    Ok(())
}

fn copy_completed_shared_rows(
    image: &ResidentMetalImage,
    buffer: &ProtocolObject<dyn MTLBuffer>,
    row_bytes: usize,
    byte_len: usize,
) -> Result<Vec<u8>, WsiError> {
    let height = image.dimensions().1 as usize;
    let span = height
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(image.pitch_bytes()))
        .and_then(|bytes| bytes.checked_add(row_bytes));
    if row_bytes > image.pitch_bytes()
        || height.checked_mul(row_bytes) != Some(byte_len)
        || span.is_none_or(|span| {
            span > image.byte_len()
                || image
                    .byte_offset()
                    .checked_add(span)
                    .is_none_or(|end| end > buffer.length())
        })
    {
        return Err(WsiError::DisplayConversion(
            "invalid Metal shared readback row span".into(),
        ));
    }
    if image.pitch_bytes() == row_bytes {
        // SAFETY: the resident image is complete and immutable; the checks above
        // prove this tight range contains exactly the initialized logical pixels.
        return unsafe {
            j2k_metal_support::checked_buffer_read_vec::<u8>(buffer, image.byte_offset(), byte_len)
        }
        .map_err(|source| support_error("metal-download-read", source));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "Metal host tile download",
            requested: byte_len as u64,
            limit: super::tile::MAX_DEVICE_DOWNLOAD_BYTES,
        })?;
    for row in 0..height {
        let offset = image.byte_offset() + row * image.pitch_bytes();
        // SAFETY: the caller proved shared CPU-visible storage. Completion and
        // immutable ownership come from ResidentMetalImage, and the complete
        // row span was checked above. Borrow only initialized logical row bytes,
        // never potentially unwritten padding, while the resident owner is alive.
        let pixels = unsafe {
            std::slice::from_raw_parts(
                buffer.contents().as_ptr().cast::<u8>().add(offset),
                row_bytes,
            )
        };
        bytes.extend_from_slice(pixels);
    }
    Ok(bytes)
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
pub(super) fn resident_private_test_image(
    device: &ProtocolObject<dyn MTLDevice>,
    bytes: &[u8],
    dimensions: (u32, u32),
    pitch: usize,
) -> ResidentMetalImage {
    let input = resident_test_image(device, bytes, dimensions, pitch);
    let buffer = j2k_metal_support::checked_private_buffer(device, bytes.len()).unwrap();
    let queue = j2k_metal_support::checked_command_queue(device).unwrap();
    let command = j2k_metal_support::checked_command_buffer(&queue).unwrap();
    let blit = j2k_metal_support::checked_blit_command_encoder(&command).unwrap();
    // SAFETY: the upload initialized all `bytes.len()` source bytes, including
    // padding. The private destination is fresh and exclusively written here.
    unsafe {
        blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
            input.raw_buffer(),
            0,
            &buffer,
            0,
            bytes.len(),
        );
    }
    blit.endEncoding();
    let layout = MetalImageLayout::new(0, dimensions, pitch, j2k_core::PixelFormat::Rgb8).unwrap();
    // SAFETY: this command is the destination's only writer and retains its
    // immutable upload source until the submitted copy has completed.
    let submitted = unsafe {
        SubmittedMetalImages::from_uncommitted(device, command, vec![(buffer, layout)], vec![input])
    }
    .unwrap();
    submitted.wait().unwrap().pop().unwrap()
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
    /// Every logical pixel byte must be initialized, and all writes to the
    /// described range must have completed. The caller must ensure no surviving
    /// raw handle mutates the allocation while the tile or
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
