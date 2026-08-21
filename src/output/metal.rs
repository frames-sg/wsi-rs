#[allow(unsafe_code)]
mod interop;
mod session;
mod tile;
mod ycbcr;

/// Owned Metal device handle used by the 0.9 J2K backend boundary.
pub type MetalDevice =
    objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDevice>>;

/// Owned Metal buffer handle accepted by the audited adoption boundary.
pub type MetalBuffer =
    objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>;

#[cfg(all(test, feature = "parity-metal"))]
pub(crate) use interop::resident_bytes;
pub use session::MetalBackendSessions;
pub use tile::{MetalDeviceStorage, MetalDeviceTile};
pub(crate) use ycbcr::YcbcrToRgb8Converter;

#[cfg(test)]
mod tests;
