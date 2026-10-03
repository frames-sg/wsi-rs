use j2k_core::BackendRequest as J2kBackendRequest;
use std::borrow::Cow;

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
pub(crate) use cpu::{
    decode_jp2k_reduced_to_sample_buffer, decode_jp2k_to_sample_buffer,
    jp2k_decodable_reduction_levels, reduced_jp2k_dimensions,
};
#[cfg(feature = "cuda")]
pub(crate) use device::decode_batch_jp2k_cuda;
#[cfg(feature = "metal")]
pub(crate) use device::decode_batch_jp2k_metal;

#[cfg(test)]
#[path = "jp2k/tests.rs"]
mod tests;
