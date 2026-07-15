#[allow(unsafe_code)]
mod interop;
mod session;
mod tile;
mod ycbcr;

pub use session::MetalBackendSessions;
pub use tile::{MetalDeviceStorage, MetalDeviceTile};
pub(crate) use ycbcr::YcbcrToRgb8Converter;

const _: () = {
    fn assert_send<T: Send>() {}
    let _ = assert_send::<MetalDeviceTile>;
};

#[cfg(test)]
mod tests;
