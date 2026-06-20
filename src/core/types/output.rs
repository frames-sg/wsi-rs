use super::*;

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DeviceOutputContext {
    #[cfg(feature = "metal")]
    metal: Option<crate::output::metal::MetalBackendSessions>,
    #[cfg(feature = "cuda")]
    cuda: Option<crate::output::cuda::CudaBackendSessions>,
    compressed_device_decode: bool,
    adaptive_decode_route: bool,
}

impl DeviceOutputContext {
    pub fn none() -> Self {
        Self {
            #[cfg(feature = "metal")]
            metal: None,
            #[cfg(feature = "cuda")]
            cuda: None,
            compressed_device_decode: false,
            adaptive_decode_route: true,
        }
    }

    #[cfg(feature = "metal")]
    pub fn with_metal(metal: crate::output::metal::MetalBackendSessions) -> Self {
        Self {
            metal: Some(metal),
            #[cfg(feature = "cuda")]
            cuda: None,
            compressed_device_decode: false,
            adaptive_decode_route: true,
        }
    }

    #[cfg(feature = "cuda")]
    pub fn with_cuda(cuda: crate::output::cuda::CudaBackendSessions) -> Self {
        Self {
            #[cfg(feature = "metal")]
            metal: None,
            cuda: Some(cuda),
            compressed_device_decode: false,
            adaptive_decode_route: true,
        }
    }

    pub fn with_compressed_device_decode(mut self) -> Self {
        self.compressed_device_decode = true;
        self
    }

    pub fn without_adaptive_decode_route(mut self) -> Self {
        self.adaptive_decode_route = false;
        self
    }

    pub(crate) fn compressed_device_decode(&self) -> bool {
        self.compressed_device_decode
    }

    pub(crate) fn adaptive_decode_route(&self) -> bool {
        self.adaptive_decode_route
    }

    #[cfg(feature = "metal")]
    pub(crate) fn metal(&self) -> Option<&crate::output::metal::MetalBackendSessions> {
        self.metal.as_ref()
    }

    #[cfg(feature = "cuda")]
    pub(crate) fn cuda(&self) -> Option<&crate::output::cuda::CudaBackendSessions> {
        self.cuda.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OutputBackendRequest {
    Auto,
    Cpu,
    #[cfg(feature = "metal")]
    Metal,
    #[cfg(feature = "cuda")]
    Cuda,
}

impl OutputBackendRequest {
    pub(crate) fn to_j2k(self) -> j2k_core::BackendRequest {
        match self {
            Self::Auto => j2k_core::BackendRequest::Auto,
            Self::Cpu => j2k_core::BackendRequest::Cpu,
            #[cfg(feature = "metal")]
            Self::Metal => j2k_core::BackendRequest::Metal,
            #[cfg(feature = "cuda")]
            Self::Cuda => j2k_core::BackendRequest::Cuda,
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TileOutputPreference {
    Cpu {
        backend: OutputBackendRequest,
    },
    PreferDevice {
        backend: OutputBackendRequest,
        context: DeviceOutputContext,
    },
    RequireDevice {
        backend: OutputBackendRequest,
        context: DeviceOutputContext,
    },
}

impl TileOutputPreference {
    /// CPU-resident output; codec picks cheapest backend that returns host pixels.
    pub fn cpu() -> Self {
        Self::Cpu {
            backend: OutputBackendRequest::Auto,
        }
    }

    /// CPU-resident output with an explicit CPU backend request.
    pub fn cpu_only() -> Self {
        Self::Cpu {
            backend: OutputBackendRequest::Cpu,
        }
    }

    /// Prefer device output, fall back to CPU silently.
    pub fn prefer_device_auto() -> Self {
        Self::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::none(),
        }
    }

    /// Require device output, letting the codec choose the device backend.
    pub fn require_device_auto() -> Self {
        Self::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::none(),
        }
    }

    /// Prefer device output and explicitly allow compressed source tile device decode.
    pub fn prefer_device_auto_with_compressed_decode() -> Self {
        Self::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::none().with_compressed_device_decode(),
        }
    }

    /// Require device output and explicitly allow compressed source tile device decode.
    pub fn require_device_auto_with_compressed_decode() -> Self {
        Self::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::none().with_compressed_device_decode(),
        }
    }

    #[cfg(feature = "metal")]
    pub fn prefer_device_auto_with_metal(
        sessions: crate::output::metal::MetalBackendSessions,
    ) -> Self {
        Self::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::with_metal(sessions),
        }
    }

    #[cfg(feature = "metal")]
    pub fn prefer_device_auto_with_metal_and_compressed_decode(
        sessions: crate::output::metal::MetalBackendSessions,
    ) -> Self {
        Self::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::with_metal(sessions).with_compressed_device_decode(),
        }
    }

    #[cfg(feature = "metal")]
    pub fn require_device_auto_with_metal_and_compressed_decode(
        sessions: crate::output::metal::MetalBackendSessions,
    ) -> Self {
        Self::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::with_metal(sessions).with_compressed_device_decode(),
        }
    }

    /// Require Metal device output. Returns Unsupported on fallback.
    #[cfg(feature = "metal")]
    pub fn require_metal() -> Self {
        Self::RequireDevice {
            backend: OutputBackendRequest::Metal,
            context: DeviceOutputContext::none(),
        }
    }

    #[cfg(feature = "cuda")]
    pub fn prefer_device_auto_with_cuda(
        sessions: crate::output::cuda::CudaBackendSessions,
    ) -> Self {
        Self::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::with_cuda(sessions),
        }
    }

    #[cfg(feature = "cuda")]
    pub fn prefer_device_auto_with_cuda_and_compressed_decode(
        sessions: crate::output::cuda::CudaBackendSessions,
    ) -> Self {
        Self::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::with_cuda(sessions).with_compressed_device_decode(),
        }
    }

    #[cfg(feature = "cuda")]
    pub fn require_device_auto_with_cuda_and_compressed_decode(
        sessions: crate::output::cuda::CudaBackendSessions,
    ) -> Self {
        Self::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context: DeviceOutputContext::with_cuda(sessions).with_compressed_device_decode(),
        }
    }

    /// Require CUDA device output. Returns Unsupported on fallback.
    #[cfg(feature = "cuda")]
    pub fn require_cuda() -> Self {
        Self::RequireDevice {
            backend: OutputBackendRequest::Cuda,
            context: DeviceOutputContext::none(),
        }
    }

    pub fn backend(&self) -> OutputBackendRequest {
        match self {
            Self::Cpu { backend }
            | Self::PreferDevice { backend, .. }
            | Self::RequireDevice { backend, .. } => *backend,
        }
    }

    pub fn requires_device(&self) -> bool {
        matches!(self, Self::RequireDevice { .. })
    }

    pub fn prefers_device(&self) -> bool {
        matches!(self, Self::PreferDevice { .. } | Self::RequireDevice { .. })
    }

    pub fn compressed_device_decode_enabled(&self) -> bool {
        match self {
            Self::PreferDevice { context, .. } | Self::RequireDevice { context, .. } => {
                context.compressed_device_decode()
            }
            Self::Cpu { .. } => false,
        }
    }

    pub fn adaptive_decode_route_enabled(&self) -> bool {
        match self {
            Self::PreferDevice { context, .. } | Self::RequireDevice { context, .. } => {
                context.adaptive_decode_route()
            }
            Self::Cpu { .. } => false,
        }
    }

    pub fn without_adaptive_decode_route(mut self) -> Self {
        match &mut self {
            Self::PreferDevice { context, .. } | Self::RequireDevice { context, .. } => {
                *context = context.clone().without_adaptive_decode_route();
            }
            Self::Cpu { .. } => {}
        }
        self
    }

    #[cfg(feature = "metal")]
    pub(crate) fn metal_sessions(&self) -> Option<&crate::output::metal::MetalBackendSessions> {
        match self {
            Self::PreferDevice { context, .. } | Self::RequireDevice { context, .. } => {
                context.metal()
            }
            Self::Cpu { .. } => None,
        }
    }

    #[cfg(feature = "cuda")]
    pub(crate) fn cuda_sessions(&self) -> Option<&crate::output::cuda::CudaBackendSessions> {
        match self {
            Self::PreferDevice { context, .. } | Self::RequireDevice { context, .. } => {
                context.cuda()
            }
            Self::Cpu { .. } => None,
        }
    }
}

/// Output payload from `SlideReader::read_tiles` and friends.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TilePixels {
    Cpu(CpuTile),
    Device(DeviceTile),
}

/// Renderer-uploadable device payload. Real payload fields land in Phase 5.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum DeviceTile {
    #[cfg(feature = "metal")]
    Metal(crate::output::metal::MetalDeviceTile),
    #[cfg(feature = "cuda")]
    Cuda(crate::output::cuda::CudaDeviceTile),
}
