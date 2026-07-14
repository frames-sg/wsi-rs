use super::*;

pub(super) enum CodecBatchJob<'a> {
    Jpeg(JpegDecodeJob<'a>),
    Jp2k(Jp2kDecodeJob<'a>),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TiffJpegDecodeOptions {
    pub(super) force_dimensions: bool,
    pub(super) color_transform: J2kColorTransform,
}

pub(super) fn decode_one_jpeg(job: JpegDecodeJob<'_>) -> Result<CpuTile, WsiError> {
    crate::core::batch::exactly_one(decode_batch_jpeg(&[job]), "TIFF JPEG decode")?
}

pub(super) fn decode_one_jp2k(job: Jp2kDecodeJob<'_>) -> Result<CpuTile, WsiError> {
    crate::core::batch::exactly_one(decode_batch_jp2k(&[job]), "TIFF JP2K decode")?
}

pub(super) fn decode_mixed_batch(
    jobs: Vec<CodecBatchJob<'_>>,
) -> Result<Vec<Result<CpuTile, WsiError>>, WsiError> {
    let mut jpeg_jobs = Vec::new();
    let mut jpeg_slots = Vec::new();
    let mut jp2k_jobs = Vec::new();
    let mut jp2k_slots = Vec::new();

    for (slot, job) in jobs.into_iter().enumerate() {
        match job {
            CodecBatchJob::Jpeg(job) => {
                jpeg_slots.push(slot);
                jpeg_jobs.push(job);
            }
            CodecBatchJob::Jp2k(job) => {
                jp2k_slots.push(slot);
                jp2k_jobs.push(job);
            }
        }
    }

    let total = jpeg_slots.len() + jp2k_slots.len();
    let mut out: Vec<Option<Result<CpuTile, WsiError>>> = (0..total).map(|_| None).collect();
    let jpeg_results = crate::core::batch::expect_exact_count(
        decode_batch_jpeg(&jpeg_jobs),
        jpeg_jobs.len(),
        "TIFF mixed JPEG decode",
    )?;
    for (slot, result) in jpeg_slots.into_iter().zip(jpeg_results) {
        out[slot] = Some(result);
    }
    let jp2k_results = crate::core::batch::expect_exact_count(
        decode_batch_jp2k(&jp2k_jobs),
        jp2k_jobs.len(),
        "TIFF mixed JP2K decode",
    )?;
    for (slot, result) in jp2k_slots.into_iter().zip(jp2k_results) {
        out[slot] = Some(result);
    }

    out.into_iter()
        .map(|result| {
            result.ok_or(WsiError::BackendContract {
                context: "TIFF mixed decode slot population",
                expected: 1,
                actual: 0,
            })
        })
        .collect()
}
