use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct CaptureDocument {
    #[serde(default)]
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) kind: String,
    #[serde(default)]
    pub(super) label: String,
    #[serde(default)]
    pub(super) repeat_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) slides: Vec<String>,
    #[serde(default)]
    pub(super) slide_manifest: Vec<SlideDeclaration>,
    #[serde(default)]
    pub(super) metadata: CaptureMetadata,
    #[serde(default)]
    pub(super) runs: Vec<CaptureRun>,
}

impl CaptureDocument {
    pub(super) fn parse(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid performance capture JSON: {error}"))
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct SlideDeclaration {
    #[serde(default)]
    pub(super) path: String,
    #[serde(default)]
    pub(super) alias: String,
    #[serde(default)]
    pub(super) format: String,
    #[serde(default)]
    pub(super) benchmark_group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) manifest_sha256: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct CaptureMetadata {
    #[serde(default)]
    pub(super) host: Value,
    #[serde(default)]
    pub(super) build: Value,
    #[serde(default)]
    pub(super) benchmark: BenchmarkMetadata,
    #[serde(flatten)]
    pub(super) extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct BenchmarkMetadata {
    #[serde(default)]
    pub(super) library: String,
    #[serde(default)]
    pub(super) cache_bytes: u64,
    #[serde(default)]
    pub(super) planned_workloads: Vec<String>,
    #[serde(default)]
    pub(super) workloads: Vec<String>,
    #[serde(default)]
    pub(super) client_worker_matrix: Vec<u64>,
    #[serde(default)]
    pub(super) physical_core_count: u64,
    #[serde(default)]
    pub(super) internal_codec_thread_budget: Value,
    #[serde(flatten)]
    pub(super) extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct CaptureRun {
    #[serde(default)]
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) kind: String,
    #[serde(default)]
    pub(super) engine: String,
    #[serde(default)]
    pub(super) library_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) library_sha256: Option<String>,
    #[serde(default)]
    pub(super) library_version: String,
    #[serde(default)]
    pub(super) slide_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) slide_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) repeat_index: Option<u64>,
    #[serde(default)]
    pub(super) cache_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) worker_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) level0_bounds: Option<wsi_rs_perf::Level0Bounds>,
    #[serde(default)]
    pub(super) levels: Vec<wsi_rs_perf::LevelResult>,
    #[serde(default)]
    pub(super) workloads: Vec<CaptureWorkload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) benchmark_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) manifest_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) engine_position: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) engine_order: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) decode_cpu_concurrency: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) peak_rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rss_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cpu_user_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cpu_system_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cpu_time_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cpu_time_method: Option<String>,
    #[serde(flatten)]
    pub(super) extra: BTreeMap<String, Value>,
}

impl CaptureRun {
    pub(super) fn from_worker(worker: wsi_rs_perf::WorkerResult) -> Self {
        Self {
            schema_version: worker.schema_version,
            kind: worker.kind,
            engine: worker.engine,
            library_path: worker.library_path,
            library_sha256: Some(worker.library_sha256),
            library_version: worker.library_version,
            slide_path: worker.slide_path,
            slide_sha256: Some(worker.slide_sha256),
            repeat_index: Some(u64::from(worker.repeat_index)),
            cache_bytes: worker.cache_bytes as u64,
            worker_count: Some(worker.worker_count as u64),
            level0_bounds: Some(worker.level0_bounds),
            levels: worker.levels,
            workloads: worker
                .workloads
                .into_iter()
                .map(CaptureWorkload::from)
                .collect(),
            ..Self::default()
        }
    }

    pub(super) fn alias(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.slide_path)
    }

    pub(super) fn format(&self) -> &str {
        self.format.as_deref().unwrap_or_else(|| self.alias())
    }

    pub(super) fn benchmark_group(&self) -> &str {
        self.benchmark_group
            .as_deref()
            .unwrap_or_else(|| self.format())
    }

    pub(super) fn worker_count(&self) -> u64 {
        self.worker_count.unwrap_or(1)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct CaptureWorkload {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) n: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) samples_us: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) p50_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) p95_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) p99_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) mean_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) bytes_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) workers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) effective_elapsed_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) throughput_bytes_per_second: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checksum_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) diagnostics: Option<Value>,
    #[serde(flatten)]
    pub(super) extra: BTreeMap<String, Value>,
}

impl CaptureWorkload {
    pub(super) fn sample_count(&self) -> Option<u64> {
        self.n.or_else(|| {
            (!self.samples_us.is_empty())
                .then(|| u64::try_from(self.samples_us.len()).unwrap_or(u64::MAX))
        })
    }

    pub(super) fn metric(&self, metric: &str) -> Option<u64> {
        match metric {
            "p50_us" => self.p50_us,
            "p95_us" => self.p95_us,
            "p99_us" => self.p99_us,
            "mean_us" => self.mean_us,
            _ => None,
        }
    }
}

impl From<wsi_rs_perf::WorkloadResult> for CaptureWorkload {
    fn from(workload: wsi_rs_perf::WorkloadResult) -> Self {
        Self {
            name: workload.name,
            n: Some(workload.n as u64),
            samples_us: workload.samples_us,
            p50_us: Some(workload.p50_us),
            p95_us: Some(workload.p95_us),
            p99_us: Some(workload.p99_us),
            mean_us: Some(workload.mean_us),
            bytes_read: Some(workload.bytes_read),
            workers: Some(workload.workers as u64),
            effective_elapsed_us: Some(workload.effective_elapsed_us),
            throughput_bytes_per_second: Some(workload.throughput_bytes_per_second),
            checksum_sha256: Some(workload.checksum_sha256),
            ..Self::default()
        }
    }
}
