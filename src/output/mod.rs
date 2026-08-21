#[cfg(feature = "cuda")]
pub mod cuda;

#[cfg(feature = "metal")]
pub mod metal;

#[cfg(feature = "metal")]
pub(crate) type MetalBackendSessionsRef<'a> = Option<&'a metal::MetalBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "metal")))]
pub(crate) type MetalBackendSessionsRef<'a> = Option<&'a ()>;
#[cfg(feature = "cuda")]
pub(crate) type CudaBackendSessionsRef<'a> = Option<&'a cuda::CudaBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "cuda")))]
pub(crate) type CudaBackendSessionsRef<'a> = Option<&'a ()>;
