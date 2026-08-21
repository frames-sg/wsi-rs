use std::borrow::Borrow;

use crate::core::types::{
    Dataset, IccProfileProvenance, SceneId, SeriesId, SourceIccProfile, SourceIccProfileKey,
};
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};

pub(crate) fn source_icc_profile_from_ifds<I>(
    container: &TiffContainer,
    ifds: I,
    scene: usize,
    series: usize,
) -> Result<Option<SourceIccProfile>, TiffParseError>
where
    I: IntoIterator,
    I::Item: Borrow<IfdId>,
{
    let mut profile: Option<SourceIccProfile> = None;

    for ifd in ifds {
        let ifd_id = *ifd.borrow();
        let bytes = match container.get_bytes(ifd_id, tags::ICC_PROFILE) {
            Ok(bytes) => bytes.to_vec(),
            Err(TiffParseError::TagNotFound { .. }) => continue,
            Err(err) => return Err(err),
        };

        if let Some(existing) = &profile {
            if existing.bytes != bytes {
                return Err(TiffParseError::Structure(format!(
                    "TIFF source contains multiple different TIFF ICC profiles in one logical series ({} vs {})",
                    source_icc_profile_location(existing),
                    source_icc_profile_location_ifd(ifd_id)
                )));
            }
            continue;
        }

        profile = Some(SourceIccProfile {
            key: SourceIccProfileKey {
                scene: SceneId::new(scene),
                series: SeriesId::new(series),
                optical_path: None,
                channel: None,
            },
            bytes,
            provenance: IccProfileProvenance::TiffTag {
                ifd_id: ifd_id.0,
                tag: tags::ICC_PROFILE,
            },
        });
    }

    Ok(profile)
}

pub(crate) fn attach_source_icc_profile<I>(
    dataset: &mut Dataset,
    container: &TiffContainer,
    ifds: I,
    scene: usize,
    series: usize,
) -> Result<(), TiffParseError>
where
    I: IntoIterator,
    I::Item: Borrow<IfdId>,
{
    if let Some(profile) = source_icc_profile_from_ifds(container, ifds, scene, series)? {
        dataset.push_source_icc_profile(profile).map_err(|err| {
            TiffParseError::Structure(format!(
                "failed to add TIFF source ICC profile to dataset: {err}"
            ))
        })?;
    }
    Ok(())
}

fn source_icc_profile_location(profile: &SourceIccProfile) -> String {
    match profile.provenance {
        IccProfileProvenance::TiffTag { ifd_id, .. } => format!("IFD@{ifd_id}"),
        _ => "unknown provenance".to_string(),
    }
}

fn source_icc_profile_location_ifd(ifd_id: IfdId) -> String {
    format!("IFD@{}", ifd_id.0)
}

#[cfg(test)]
#[path = "icc/tests.rs"]
mod tests;
