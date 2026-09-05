use super::*;
use std::collections::HashMap;
use std::ffi::CString;
use std::panic::{catch_unwind, AssertUnwindSafe};
use wsi_rs::{
    AssociatedImage, AxesShape, CpuTile, Dataset, DatasetId, Level, SampleType, Scene, Series,
    SlideReader, TileRequest, WsiError,
};

struct MetadataOnlySource {
    dataset: Dataset,
}

impl SlideReader for MetadataOnlySource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        unreachable!("metadata-only test never reads pixels")
    }
}

#[test]
fn tile_size_uses_virtual_size_for_whole_level_layout() {
    let layout = TileLayout::WholeLevel {
        width: 1024,
        height: 768,
        virtual_tile_width: 512,
        virtual_tile_height: 256,
    };

    assert_eq!(tile_size(&layout), Some((512, 256)));
}

#[test]
fn tile_size_rounds_irregular_tile_advance() {
    let layout = TileLayout::Irregular {
        tile_advance: (127.6, 0.2),
        extra_tiles: (0, 0, 0, 0),
        tiles: HashMap::new(),
    };

    assert_eq!(tile_size(&layout), Some((128, 1)));
}

#[test]
fn handle_error_state_recovers_from_poisoned_mutex() {
    let handle = OpenSlideHandle::from_error("initial error".to_string());

    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = handle.error.lock().expect("lock error mutex");
        panic!("poison error mutex");
    }));

    handle.set_error("later error");

    assert!(handle.has_error());
    assert!(!handle.error_ptr().is_null());
    assert_eq!(handle.property_names(), empty_names());
}

#[test]
fn terminal_error_reads_clear_known_output_buffers() {
    let mut handle = OpenSlideHandle::from_error("terminal error".to_string());
    handle.icc_profile = vec![1, 2, 3];
    handle.associated_images.insert(
        "label".to_string(),
        AssociatedImageInfo {
            width: 2,
            height: 1,
            icc_profile: Vec::new(),
        },
    );
    let label = CString::new("label").unwrap();
    let osr = Box::into_raw(Box::new(handle)).cast::<crate::openslide_t>();

    // SAFETY: `osr` owns the boxed handle above until the final close, and
    // both destinations match the sizes stored in that handle.
    unsafe {
        let mut profile = [u8::MAX; 3];
        crate::openslide_read_icc_profile(osr, profile.as_mut_ptr().cast());
        assert_eq!(profile, [0; 3]);

        let mut pixels = [u32::MAX; 2];
        crate::openslide_read_associated_image(osr, label.as_ptr(), pixels.as_mut_ptr());
        assert_eq!(pixels, [0; 2]);

        crate::openslide_close(osr);
    }
}

#[test]
fn zero_length_associated_icc_read_accepts_null_destination() {
    let mut handle = OpenSlideHandle::from_error("temporary".to_string());
    *handle.error.lock().unwrap() = None;
    handle.associated_images.insert(
        "label".to_string(),
        AssociatedImageInfo {
            width: 2,
            height: 1,
            icc_profile: Vec::new(),
        },
    );
    let label = CString::new("label").unwrap();
    let osr = Box::into_raw(Box::new(handle)).cast::<crate::openslide_t>();

    // SAFETY: `osr` owns the boxed handle until the final close. This shim
    // reports a zero-byte associated ICC profile, so a null destination is
    // valid and is never dereferenced.
    unsafe {
        crate::openslide_read_associated_image_icc_profile(
            osr,
            label.as_ptr(),
            std::ptr::null_mut(),
        );
        assert!(crate::openslide_get_error(osr).is_null());
        crate::openslide_close(osr);
    }
}

#[test]
fn associated_icc_profile_is_reported_and_copied() {
    let mut handle = OpenSlideHandle::from_error("temporary".to_string());
    *handle.error.lock().unwrap() = None;
    handle.associated_images.insert(
        "label".to_string(),
        AssociatedImageInfo {
            width: 2,
            height: 1,
            icc_profile: vec![3, 1, 4, 1],
        },
    );
    let label = CString::new("label").unwrap();
    let osr = Box::into_raw(Box::new(handle)).cast::<crate::openslide_t>();

    // SAFETY: `osr` owns the boxed handle until close, and `profile` has the
    // exact size reported by the preceding ABI query.
    unsafe {
        assert_eq!(
            crate::openslide_get_associated_image_icc_profile_size(osr, label.as_ptr()),
            4
        );
        let mut profile = [0_u8; 4];
        crate::openslide_read_associated_image_icc_profile(
            osr,
            label.as_ptr(),
            profile.as_mut_ptr().cast(),
        );
        assert_eq!(profile, [3, 1, 4, 1]);
        assert!(crate::openslide_get_error(osr).is_null());
        crate::openslide_close(osr);
    }
}

#[test]
fn associated_image_metadata_is_not_synthesized_as_openslide_properties() {
    let level = Level::new(
        (1, 1),
        1.0,
        TileLayout::WholeLevel {
            width: 1,
            height: 1,
            virtual_tile_width: 1,
            virtual_tile_height: 1,
        },
    );
    let series = Series::new(
        "series",
        AxesShape::default(),
        vec![level],
        SampleType::Uint8,
        Vec::new(),
    );
    let dataset = Dataset::new(DatasetId::new(1), vec![Scene::new("scene", vec![series])])
        .with_associated_images(HashMap::from([(
            "label".to_string(),
            AssociatedImage::new((12, 34), SampleType::Uint8, 3).with_icc_profile(vec![3, 1, 4, 1]),
        )]));
    let slide = Slide::from_source_with_cache_bytes(Box::new(MetadataOnlySource { dataset }), 4096);
    let handle = OpenSlideHandle::from_slide(slide);

    assert!(!handle
        .properties
        .keys()
        .any(|name| name.starts_with("openslide.associated.")));
    let label = &handle.associated_images["label"];
    assert_eq!((label.width, label.height), (12, 34));
    assert_eq!(label.icc_profile, [3, 1, 4, 1]);
}

#[cfg(feature = "route-telemetry")]
#[test]
fn internal_route_telemetry_property_is_dynamic_but_not_advertised() {
    let handle = OpenSlideHandle::from_error("temporary".to_string());
    *handle.error.lock().unwrap() = None;
    let property = CString::new(ROUTE_TELEMETRY_PROPERTY).unwrap();

    let value = handle.property_value(&property);

    assert!(!value.is_null());
    assert!(!handle.property_value(&property).is_null());
    // SAFETY: `value` points into `handle.route_telemetry_values`, which
    // retains all returned snapshots for the handle lifetime, including
    // across the second query above.
    let value = unsafe { std::ffi::CStr::from_ptr(value) };
    let value = value.to_str().unwrap();
    assert!(value.contains("\"metal\""));
    assert!(value.contains("\"cuda\""));
    assert_eq!(value.matches("\"device_attempt_tiles\"").count(), 2);
    assert!(!handle.properties.contains_key(ROUTE_TELEMETRY_PROPERTY));
}
