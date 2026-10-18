use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::{num::NonZeroUsize, sync::MutexGuard};

use crate::error::WsiError;
use objc2_metal::MTLDevice;

use super::MetalDeviceTile;
use super::{MetalDevice, YcbcrToRgb8Converter};

/// Metal session allocated for JP2K and HTJ2K device decode.
#[derive(Debug, Clone)]
pub struct MetalBackendSessions {
    pub(crate) j2k: Arc<j2k_metal::MetalBackendSession>,
    ycbcr_to_rgb8: Arc<Mutex<Option<Arc<YcbcrToRgb8Converter>>>>,
    readback_queue: Arc<super::interop::ReadbackQueueCache>,
    #[cfg(target_os = "macos")]
    cooperative_cpu_workers: Option<NonZeroUsize>,
    #[cfg(target_os = "macos")]
    cooperative_decode_slot: Arc<Mutex<()>>,
}

impl MetalBackendSessions {
    pub fn new(device: MetalDevice) -> Self {
        Self::from_session(j2k_metal::MetalBackendSession::new(device))
    }

    /// Create codec sessions on the system default Metal device.
    pub fn system_default() -> Result<Self, WsiError> {
        j2k_metal_support::system_default_device()
            .map(Self::new)
            .map_err(|source| super::interop::support_error("metal-session", source))
    }

    pub(crate) fn from_session(j2k: j2k_metal::MetalBackendSession) -> Self {
        Self {
            j2k: Arc::new(j2k),
            ycbcr_to_rgb8: Arc::new(Mutex::new(None)),
            readback_queue: Arc::new(super::interop::ReadbackQueueCache::default()),
            #[cfg(target_os = "macos")]
            cooperative_cpu_workers: None,
            #[cfg(target_os = "macos")]
            cooperative_decode_slot: Arc::new(Mutex::new(())),
        }
    }

    /// Opts eligible JPEG 2000 batches into overlapping CPU entropy decoding
    /// with Metal. The worker count is capped by the WSI CPU pool's width.
    ///
    /// Clones share one nonblocking cooperative slot: concurrent batches use
    /// ordinary Metal while it is occupied. A budget of one, small batches,
    /// and unsupported codec profiles also retain ordinary Metal decoding.
    /// Existing batch size and memory limits still apply.
    ///
    /// This mode is disabled by default and is not guaranteed to reduce
    /// latency. It applies to reads using this session; automatic slide
    /// acceleration keeps its own default session and calibration policy.
    #[cfg(target_os = "macos")]
    pub fn with_cooperative_cpu_workers(mut self, workers: NonZeroUsize) -> Self {
        self.cooperative_cpu_workers = Some(workers);
        self
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn try_cooperative_decode(&self) -> Option<(NonZeroUsize, MutexGuard<'_, ()>)> {
        let requested = self.cooperative_cpu_workers?;
        let workers = requested
            .get()
            .min(crate::core::decode_runtime::DecodeRuntime::default_arc().cpu_worker_count());
        if workers < 2 {
            return None;
        }
        Some((
            NonZeroUsize::new(workers)?,
            self.cooperative_decode_slot.try_lock().ok()?,
        ))
    }

    /// Builds this session's JPEG 2000 decode kernels now, so the first slide
    /// read does not wait for shader compilation. Call it from a background
    /// thread at startup. Sessions on the same device share the kernels while
    /// this session lives.
    pub fn prewarm(&self) -> Result<(), WsiError> {
        // A 16x12 RGB codestream: the smallest decode that builds every kernel.
        const CODESTREAM: &[u8] = include_bytes!("prewarm.j2k");
        let job = crate::decode::jp2k::Jp2kDecodeJob {
            data: std::borrow::Cow::Borrowed(CODESTREAM),
            expected_width: 16,
            expected_height: 12,
            rgb_color_space: true,
            backend: j2k_core::BackendRequest::Metal,
        };
        crate::core::batch::exactly_one(
            crate::decode::jp2k::decode_batch_jp2k_metal(std::slice::from_ref(&job), self),
            "Metal prewarm decode",
        )?
        .map(drop)
    }

    pub(crate) fn j2k(&self) -> &j2k_metal::MetalBackendSession {
        &self.j2k
    }

    pub(crate) fn device_identity(&self) -> String {
        #[cfg(target_os = "macos")]
        {
            format!(
                "metal:{}:{}",
                self.j2k.device().registryID(),
                self.j2k.device().name()
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            "metal".to_string()
        }
    }

    pub(crate) fn ycbcr_to_rgb8_converter(&self) -> Result<Arc<YcbcrToRgb8Converter>, WsiError> {
        let mut cached = self
            .ycbcr_to_rgb8
            .lock()
            .map_err(|_| WsiError::Unsupported {
                reason: "Metal YCbCr converter cache lock is poisoned".into(),
            })?;
        if let Some(converter) = cached.as_ref() {
            return Ok(converter.clone());
        }

        let converter = Arc::new(YcbcrToRgb8Converter::new(self.j2k())?);
        *cached = Some(converter.clone());
        Ok(converter)
    }

    pub(crate) fn ycbcr8_tiles_to_rgb8(
        &self,
        tiles: &[MetalDeviceTile],
    ) -> Result<Vec<MetalDeviceTile>, WsiError> {
        self.ycbcr_to_rgb8_converter()?.convert_tiles(tiles)
    }

    pub(crate) fn retain_readback_queue(
        &self,
        mut tile: MetalDeviceTile,
    ) -> Result<MetalDeviceTile, WsiError> {
        tile.resident_image_for_device(self.j2k().device())?;
        tile.readback_queue = std::sync::OnceLock::from(self.readback_queue.clone());
        Ok(tile)
    }

    pub(crate) fn download_cpu_batch(
        &self,
        tiles: &[MetalDeviceTile],
    ) -> Result<Vec<crate::CpuTile>, WsiError> {
        use crate::output::download::{downloaded_bytes_to_cpu_tile, tight_download_layout};
        let rows = tiles
            .iter()
            .map(|tile| {
                let image = tile.resident_image_for_device(self.j2k().device())?;
                let (row_bytes, byte_len) =
                    tight_download_layout(tile.width, tile.height, tile.format, "Metal")?;
                super::tile::enforce_download_limit(byte_len)?;
                Ok(super::interop::ReadbackRows {
                    image,
                    row_bytes,
                    byte_len,
                })
            })
            .collect::<Result<Vec<_>, WsiError>>()?;
        super::interop::download_resident_batch(&rows, &self.readback_queue)?
            .into_iter()
            .zip(tiles)
            .map(|(bytes, tile)| {
                downloaded_bytes_to_cpu_tile(tile.width, tile.height, tile.format, bytes, "Metal")
            })
            .collect()
    }
}
