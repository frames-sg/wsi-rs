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
fn prewarm_builds_decode_kernels_that_later_decodes_reuse() {
    let Some(device) = test_device() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device);
    sessions.prewarm().expect("prewarm decode");
    // A second prewarm reuses the published kernels.
    sessions.prewarm().expect("prewarm again");
}

#[test]
fn metal_device_tile_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<MetalDeviceTile>();
}
