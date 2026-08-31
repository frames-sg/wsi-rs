use std::path::Path;

use crate::error::WsiError;

use super::model::DicomFragmentRef;
use crate::formats::dicom::metadata::invalid_slide;

/// Validate one compressed frame's complete fragment graph before any payload
/// buffer is reserved, copied, or passed to a decoder.
pub(in super::super) fn preflight_compressed_frame(
    path: &Path,
    fragments: &[DicomFragmentRef],
) -> Result<usize, WsiError> {
    preflight_compressed_frame_with_limit(
        path,
        fragments,
        crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
    )
}

pub(in super::super) fn preflight_compressed_frame_with_limit(
    path: &Path,
    fragments: &[DicomFragmentRef],
    limit: u64,
) -> Result<usize, WsiError> {
    if fragments.is_empty() {
        return Err(invalid_slide(
            path,
            "compressed DICOM frame has no fragments",
        ));
    }
    for fragment in fragments {
        let expected_payload_offset = fragment
            .item_offset
            .checked_add(8)
            .ok_or_else(|| invalid_slide(path, "DICOM fragment Item offset overflow"))?;
        if expected_payload_offset != fragment.payload_offset {
            return Err(invalid_slide(
                path,
                "DICOM fragment payload offset does not follow its Item header",
            ));
        }
        fragment
            .payload_offset
            .checked_add(u64::from(fragment.len))
            .ok_or_else(|| invalid_slide(path, "DICOM fragment payload offset overflow"))?;
    }
    preflight_compressed_lengths_with_limit(
        path,
        fragments.iter().map(|fragment| fragment.len as usize),
        limit,
    )
}

pub(in super::super) fn preflight_compressed_lengths_with_limit(
    path: &Path,
    lengths: impl IntoIterator<Item = usize>,
    limit: u64,
) -> Result<usize, WsiError> {
    let mut total = 0u64;
    for length in lengths {
        let length = u64::try_from(length)
            .map_err(|_| invalid_slide(path, "DICOM compressed fragment length overflow"))?;
        if length > limit {
            return Err(WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: length,
                limit,
            });
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| invalid_slide(path, "DICOM compressed frame length overflow"))?;
        if total > limit {
            return Err(WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: total,
                limit,
            });
        }
    }
    usize::try_from(total)
        .map_err(|_| invalid_slide(path, "DICOM compressed frame is not addressable"))
}
