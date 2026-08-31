use super::ycbcr::{YcbcrAddressPlan, YcbcrAddressWidth, YcbcrToRgb8Params, YCBCR_TO_RGB8_METAL};
use super::*;
use crate::{error::WsiError, PixelFormat};

use super::interop::{resident_bytes, resident_test_image, u64_buffer_values};

mod address;
mod conversion;
mod download;
mod perf;

fn test_device() -> Option<MetalDevice> {
    j2k_metal_support::system_default_device().ok()
}

fn ycbcr_test_tile(device: &MetalDevice, bytes: &[u8]) -> MetalDeviceTile {
    MetalDeviceTile::from_resident(resident_test_image(device, bytes, (2, 1), 6))
        .expect("resident test tile")
}

#[test]
fn metal_device_tile_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<MetalDeviceTile>();
}
