use j2k_core::BackendRequest as J2kBackendRequest;
use std::borrow::Cow;

#[cfg(test)]
use crate::core::types::CpuTile;
#[cfg(all(any(feature = "metal", feature = "cuda"), test))]
use crate::core::types::PixelFormat;
#[cfg(test)]
use crate::error::WsiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Jp2kColorSpace {
    Rgb,
    YCbCr,
}

#[derive(Debug, Clone)]
pub(crate) struct Jp2kDecodeJob<'a> {
    pub data: Cow<'a, [u8]>,
    pub expected_width: u32,
    pub expected_height: u32,
    pub rgb_color_space: bool,
    pub backend: J2kBackendRequest,
}

mod batch;
mod cpu;
#[cfg(feature = "cuda")]
mod cuda;
#[cfg(any(feature = "metal", feature = "cuda"))]
mod device;
#[cfg(feature = "metal")]
#[path = "jp2k/metal.rs"]
mod metal_backend;
#[cfg(all(feature = "metal", target_os = "macos"))]
mod metal_batch;
mod output;
mod prepare;
#[cfg(any(feature = "metal", feature = "cuda"))]
mod prepared_batch;
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(crate) use prepared_batch::PreparedJp2kBatch;

pub(crate) use batch::decode_batch_jp2k;
#[cfg(test)]
pub(crate) use batch::decode_jp2k_tile_batch_to_sample_buffers;
pub(crate) use cpu::decode_jp2k_to_sample_buffer;
#[cfg(feature = "cuda")]
pub(crate) use device::decode_batch_jp2k_cuda;
#[cfg(feature = "metal")]
pub(crate) use device::decode_batch_jp2k_metal;

#[cfg(test)]
use batch::{
    decode_jp2k_tile_batch_with_j2k, materialize_jp2k_batch_outputs,
    try_decode_batch_jp2k_with_j2k, PreparedJp2kBatchJob,
};
#[cfg(all(feature = "cuda", test))]
use device::decode_one_jp2k_cuda;
#[cfg(all(feature = "metal", test))]
use device::decode_one_jp2k_metal;
#[cfg(test)]
#[path = "jp2k/tests.rs"]
mod tests;
