//! Shared, operation-local codec plans for unbiased CPU/device comparisons.
use super::prepare::{prepare_jp2k_job, PreparedJp2kJob};
use super::{Jp2kColorSpace, Jp2kDecodeJob};
use crate::{CpuTile, WsiError};
use j2k::{BatchDecodeOptions, BatchLayout, EncodedImage, PreparedBatch};
use j2k_core::BackendRequest;
use std::sync::Arc;

struct OwnedJob {
    bytes: Arc<[u8]>,
    dimensions: (u32, u32),
    logical: (u32, u32),
    colorspace: Jp2kColorSpace,
}

impl OwnedJob {
    fn borrowed(&self, backend: BackendRequest) -> PreparedJp2kJob<'_> {
        PreparedJp2kJob {
            input: &self.bytes,
            decoded_width: self.dimensions.0,
            decoded_height: self.dimensions.1,
            expected_width: self.logical.0,
            expected_height: self.logical.1,
            output_colorspace: self.colorspace,
            row_bytes: self.dimensions.0 as usize * 3,
            output_len: self.dimensions.0 as usize * self.dimensions.1 as usize * 3,
            backend,
        }
    }
}

pub(crate) struct PreparedJp2kBatch {
    jobs: Vec<OwnedJob>,
    prepared: PreparedBatch,
    sequential_cpu_images: bool,
}

impl PreparedJp2kBatch {
    pub(crate) fn new(jobs: &[Jp2kDecodeJob<'_>], workers: usize) -> Result<Self, WsiError> {
        let mut owners = std::collections::HashMap::new();
        let jobs = jobs
            .iter()
            .map(|job| {
                let prepared = prepare_jp2k_job(job)?;
                let dimensions = (prepared.decoded_width, prepared.decoded_height);
                let logical = (prepared.expected_width, prepared.expected_height);
                let colorspace = prepared.output_colorspace;
                Ok(OwnedJob {
                    bytes: owners
                        .entry((job.data.as_ptr(), job.data.len()))
                        .or_insert_with(|| Arc::<[u8]>::from(job.data.as_ref()))
                        .clone(),
                    dimensions,
                    logical,
                    colorspace,
                })
            })
            .collect::<Result<Vec<_>, WsiError>>()?;
        let options = BatchDecodeOptions {
            layout: BatchLayout::Nhwc,
            workers: std::num::NonZeroUsize::new(workers),
            ..BatchDecodeOptions::default()
        };
        let prepared = j2k::prepare_batch(
            jobs.iter()
                .map(|job| EncodedImage::full(job.bytes.clone()))
                .collect(),
            options,
        )
        .map_err(|error| WsiError::Jp2k(error.to_string()))?;
        Ok(Self {
            jobs,
            prepared,
            sequential_cpu_images: false,
        })
    }

    pub(crate) fn with_sequential_cpu_images(mut self) -> Self {
        self.sequential_cpu_images = true;
        self
    }

    pub(crate) fn extra_work_bytes(&self) -> u64 {
        self.jobs.iter().fold(0_u64, |sum, job| {
            sum.saturating_add(job.bytes.len() as u64).saturating_add(
                u64::from(job.dimensions.0)
                    .saturating_mul(u64::from(job.dimensions.1))
                    .saturating_mul(3 * 3),
            )
        })
    }

    pub(crate) fn read_cpu(&self) -> Result<Vec<CpuTile>, WsiError> {
        use crate::core::execution_telemetry::{record, Event};
        record(Event::CpuJp2kBatches, 1);
        record(Event::CpuJp2kTiles, self.jobs.len());
        if self.sequential_cpu_images {
            // Raw JP2K's ordinary reader decodes one image at a time while
            // permitting intra-image parallelism. Calibration must time that
            // same strategy, including duplicate-image requests.
            return self
                .jobs
                .iter()
                .map(|job| {
                    super::cpu::decode_prepared_jp2k_job(
                        &job.borrowed(BackendRequest::Cpu),
                        j2k::CpuDecodeParallelism::Auto,
                    )
                })
                .collect();
        }
        // j2k 0.10.0's owned CPU path changes lossy rounding and regresses
        // subsampled tile batches. Reuse the established executor and validated
        // metadata over the same immutable inputs retained for Metal preparation.
        super::batch::decode_prepared_jobs(
            self.jobs
                .iter()
                .map(|job| Ok(job.borrowed(BackendRequest::Cpu)))
                .collect(),
        )
        .into_iter()
        .collect()
    }

    #[cfg(feature = "metal")]
    pub(crate) fn read_metal(
        &self,
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<CpuTile>, WsiError> {
        #[cfg(target_os = "macos")]
        let tiles = {
            let metadata = self
                .jobs
                .iter()
                .map(|job| Some(job.borrowed(BackendRequest::Metal)))
                .collect::<Vec<_>>();
            let mut output = (0..self.jobs.len()).map(|_| None).collect::<Vec<_>>();
            let slots = (0..self.jobs.len()).collect::<Vec<_>>();
            super::metal_batch::execute_prepared_bounded(
                &self.prepared,
                &slots,
                &metadata,
                &mut output,
                sessions,
                super::metal_batch::GROUP_OUTPUT_TARGET,
            )?;
            super::metal_batch::convert_outputs(&metadata, &mut output, sessions);
            output
                .into_iter()
                .map(|tile| {
                    tile.unwrap_or_else(|| {
                        Err(WsiError::Jp2k("prepared Metal batch omitted input".into()))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        #[cfg(not(target_os = "macos"))]
        let tiles = self
            .jobs
            .iter()
            .map(|job| {
                super::metal_backend::decode_prepared_jp2k_metal(
                    &job.borrowed(BackendRequest::Metal),
                    sessions,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        sessions.download_cpu_batch(&tiles)
    }

    #[cfg(feature = "cuda")]
    pub(crate) fn read_cuda(
        &self,
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.jobs
            .iter()
            .map(|job| {
                super::cuda::decode_prepared_jp2k_cuda(
                    &job.borrowed(BackendRequest::Cuda),
                    sessions,
                )?
                .download_cpu()
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "tests/prepared_performance.rs"]
mod performance;
