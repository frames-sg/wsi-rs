use super::*;
use crate::core::types::{IccProfileProvenance, SourceIccProfileKey};
use crate::formats::tiff_family::container::TiffContainer;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

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
