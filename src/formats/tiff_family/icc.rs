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
mod tests {
    use super::*;
    use crate::core::types::{IccProfileProvenance, SourceIccProfileKey};
    use crate::formats::tiff_family::container::TiffContainer;
    use std::io::Write;
    use tempfile::NamedTempFile;

    struct SyntheticTag {
        tag: u16,
        tiff_type: u16,
        count: u32,
        inline_value: [u8; 4],
        ool_data: Option<Vec<u8>>,
    }

    impl SyntheticTag {
        fn long(tag: u16, value: u32) -> Self {
            Self {
                tag,
                tiff_type: 4,
                count: 1,
                inline_value: value.to_le_bytes(),
                ool_data: None,
            }
        }

        fn bytes(tag: u16, data: Vec<u8>) -> Self {
            Self {
                tag,
                tiff_type: 7,
                count: data.len() as u32,
                inline_value: [0; 4],
                ool_data: Some(data),
            }
        }
    }

    fn build_tiff(ifds: &[Vec<SyntheticTag>]) -> NamedTempFile {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        let first_ifd_offset_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());

        let mut ool_offsets = Vec::new();
        for (ifd_idx, tags) in ifds.iter().enumerate() {
            for (tag_idx, tag) in tags.iter().enumerate() {
                if let Some(data) = &tag.ool_data {
                    let offset = buf.len() as u32;
                    buf.extend_from_slice(data);
                    ool_offsets.push((ifd_idx, tag_idx, offset));
                }
            }
        }

        let mut ifd_offsets = Vec::new();
        let mut next_ifd_patch_positions = Vec::new();
        for (ifd_idx, tags) in ifds.iter().enumerate() {
            let ifd_offset = buf.len() as u32;
            ifd_offsets.push(ifd_offset);

            let mut sorted = tags.iter().enumerate().collect::<Vec<_>>();
            sorted.sort_by_key(|(_, tag)| tag.tag);

            buf.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
            for (orig_idx, tag) in sorted {
                buf.extend_from_slice(&tag.tag.to_le_bytes());
                buf.extend_from_slice(&tag.tiff_type.to_le_bytes());
                buf.extend_from_slice(&tag.count.to_le_bytes());
                if tag.ool_data.is_some() {
                    let offset = ool_offsets
                        .iter()
                        .find(|(ii, ti, _)| *ii == ifd_idx && *ti == orig_idx)
                        .map(|(_, _, offset)| *offset)
                        .unwrap();
                    buf.extend_from_slice(&offset.to_le_bytes());
                } else {
                    buf.extend_from_slice(&tag.inline_value);
                }
            }

            let next_pos = buf.len();
            buf.extend_from_slice(&0u32.to_le_bytes());
            next_ifd_patch_positions.push(next_pos);
        }

        buf[first_ifd_offset_pos..first_ifd_offset_pos + 4]
            .copy_from_slice(&ifd_offsets[0].to_le_bytes());
        for idx in 0..ifd_offsets.len().saturating_sub(1) {
            let pos = next_ifd_patch_positions[idx];
            buf[pos..pos + 4].copy_from_slice(&ifd_offsets[idx + 1].to_le_bytes());
        }

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file
    }

    fn container_with_icc_profiles(profiles: &[Option<Vec<u8>>]) -> TiffContainer {
        let ifds = profiles
            .iter()
            .enumerate()
            .map(|(idx, profile)| {
                let mut tags = vec![
                    SyntheticTag::long(tags::IMAGE_WIDTH, 1024 / (idx as u32 + 1)),
                    SyntheticTag::long(tags::IMAGE_LENGTH, 768 / (idx as u32 + 1)),
                ];
                if let Some(bytes) = profile {
                    tags.push(SyntheticTag::bytes(tags::ICC_PROFILE, bytes.clone()));
                }
                tags
            })
            .collect::<Vec<_>>();
        let file = build_tiff(&ifds);
        TiffContainer::open(file.path()).unwrap()
    }

    #[test]
    fn no_icc_returns_none() {
        let container = container_with_icc_profiles(&[None, None]);
        let profile = source_icc_profile_from_ifds(&container, container.top_ifds(), 0, 0).unwrap();

        assert!(profile.is_none());
    }

    #[test]
    fn one_icc_preserves_exact_bytes_and_provenance() {
        let icc_bytes = vec![0, 1, 2, 3, 0, 255];
        let container = container_with_icc_profiles(&[Some(icc_bytes.clone()), None]);
        let ifd_id = container.top_ifds()[0];

        let profile = source_icc_profile_from_ifds(&container, container.top_ifds(), 2, 3)
            .unwrap()
            .unwrap();

        assert_eq!(
            profile.key,
            SourceIccProfileKey {
                scene: SceneId::new(2),
                series: SeriesId::new(3),
                optical_path: None,
                channel: None,
            }
        );
        assert_eq!(profile.bytes, icc_bytes);
        assert_eq!(
            profile.provenance,
            IccProfileProvenance::TiffTag {
                ifd_id: ifd_id.0,
                tag: tags::ICC_PROFILE,
            }
        );
    }

    #[test]
    fn identical_duplicate_icc_profiles_return_one_profile() {
        let icc_bytes = vec![9, 8, 7, 6, 5];
        let container =
            container_with_icc_profiles(&[Some(icc_bytes.clone()), Some(icc_bytes.clone())]);

        let profile = source_icc_profile_from_ifds(&container, container.top_ifds(), 0, 0)
            .unwrap()
            .unwrap();

        assert_eq!(profile.bytes, icc_bytes);
    }

    #[test]
    fn different_duplicate_icc_profiles_return_structure_error() {
        let container =
            container_with_icc_profiles(&[Some(vec![1, 2, 3, 4, 5]), Some(vec![4, 5, 6, 7, 8])]);

        let err = source_icc_profile_from_ifds(&container, container.top_ifds(), 0, 0).unwrap_err();

        match err {
            TiffParseError::Structure(message) => {
                assert!(message.contains("multiple different TIFF ICC profiles"));
            }
            other => panic!("expected Structure error, got {other:?}"),
        }
    }
}
