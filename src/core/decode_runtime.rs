use crate::core::registry::{ManagedSlideReader, SlideReader};
use crate::core::types::{CpuTile, TileCodecKind, TileRequest};
use crate::error::WsiError;
use std::sync::{Arc, OnceLock};

/// Where reads that return tiles in CPU memory decode.
///
/// Those reads decode on the CPU. On slide tiles the multithreaded CPU
/// decoder outpaces Metal for JPEG and classic JPEG 2000 once the GPU's output
/// is read back (see `docs/architecture.md`, "GPU decoding"). To decode on a
/// GPU and keep the tiles there, call `Slide::read_tile_metal`,
/// `Slide::read_tiles_metal`, `Slide::read_tile_cuda` or
/// `Slide::read_tiles_cuda`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeAcceleration {
    /// wsi-rs chooses where to decode. Every codec currently decodes on the
    /// CPU; a later minor release may move a codec to an available GPU where
    /// measurements show a win.
    Auto,
    /// Always decode on the CPU, in this and later releases.
    CpuOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeExecutionOptions {
    acceleration: DecodeAcceleration,
}

impl DecodeExecutionOptions {
    pub fn with_acceleration(mut self, acceleration: DecodeAcceleration) -> Self {
        self.acceleration = acceleration;
        self
    }

    pub fn acceleration(&self) -> DecodeAcceleration {
        self.acceleration
    }
}

impl Default for DecodeExecutionOptions {
    fn default() -> Self {
        Self {
            acceleration: DecodeAcceleration::Auto,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DecodeRuntime {
    options: DecodeExecutionOptions,
}

impl DecodeRuntime {
    pub(crate) fn arc_for_options(options: DecodeExecutionOptions) -> Result<Arc<Self>, WsiError> {
        static AUTO_RUNTIME: OnceLock<Arc<DecodeRuntime>> = OnceLock::new();
        static CPU_ONLY_RUNTIME: OnceLock<Arc<DecodeRuntime>> = OnceLock::new();
        let runtime = match options.acceleration {
            DecodeAcceleration::Auto => AUTO_RUNTIME.get_or_init(|| Arc::new(Self::build(options))),
            DecodeAcceleration::CpuOnly => {
                CPU_ONLY_RUNTIME.get_or_init(|| Arc::new(Self::build(options)))
            }
        };
        Ok(Arc::clone(runtime))
    }

    fn build(options: DecodeExecutionOptions) -> Self {
        Self { options }
    }

    pub(crate) fn default_arc() -> Arc<Self> {
        Self::arc_for_options(DecodeExecutionOptions::default())
            .expect("constructing the default decode runtime is infallible")
    }

    pub(crate) fn install_codec_cpu<R: Send>(&self, operation: impl FnOnce() -> R + Send) -> R {
        crate::core::batch::install_codec_cpu(operation)
    }

    pub(crate) fn cpu_worker_count(&self) -> usize {
        crate::core::batch::cpu_worker_count()
    }

    pub(crate) fn options(&self) -> DecodeExecutionOptions {
        self.options
    }
}

/// Decodes CPU-returning tile reads on the CPU, running JPEG 2000 batches on
/// the codec pool. Every [`DecodeAcceleration`] currently routes here.
pub(crate) struct AdaptiveDecodeReader {
    inner: Box<dyn ManagedSlideReader>,
    runtime: Arc<DecodeRuntime>,
}

/// Serialize cumulative process-wide decode path counters.
///
/// `execution` maps each counter name to its total. `metal_batch_submissions`,
/// `metal_single_decodes` and `cuda_decode_jobs` count GPU decodes, which only
/// the GPU-resident read methods make. This benchmark-only surface is
/// intentionally JSON rather than public types so normal consumers do not
/// acquire another compatibility API.
#[cfg(feature = "route-telemetry")]
#[doc(hidden)]
pub fn decode_route_telemetry_json() -> String {
    serde_json::json!({
        "execution": crate::core::execution_telemetry::snapshot(),
    })
    .to_string()
}

mod reader;

#[cfg(test)]
#[path = "decode_runtime/tests.rs"]
mod tests;
