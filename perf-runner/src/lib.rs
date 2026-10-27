mod config;
mod runner;
mod workload;

pub use config::{Engine, WorkerConfig, DEFAULT_CACHE_BYTES, PINNED_OPENSLIDE_VERSION};
pub use runner::{
    compare_pixels, run, sha256_file, LevelResult, PixelComparison, WorkerResult,
    WorkloadPixelComparison, WorkloadResult, WORKER_SCHEMA_VERSION,
};
pub use workload::{
    percentile, summarize_samples, Level0Bounds, LevelInfo, ReadSpec, SampleSummary, Workload,
    WorkloadPlan, CAPTURE_WORKLOAD_NAMES,
};
