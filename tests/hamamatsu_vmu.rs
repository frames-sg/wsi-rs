//! Synthetic NGR fixtures exercise the published column-major storage contract.
use std::path::Path;
use wsi_rs::{CpuTileData, LevelIdx, RegionRequest, SceneId, SeriesId, Slide};

fn sample(x: u32, y: u32, c: u32) -> u16 {
    ((x * 251 + y * 37 + c * 997) % 4096) as u16
}

fn write_ngr(path: &Path, width: u32, height: u32, column: u32) {
    let mut bytes = vec![0; 40];
    bytes[..2].copy_from_slice(b"GN");
    for (offset, value) in [(4, width), (8, height), (12, column), (24, 40)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for col in 0..width / column {
        for y in 0..height {
            for x in col * column..(col + 1) * column {
                for c in 0..3 {
                    bytes.extend_from_slice(&sample(x, y, c).to_le_bytes());
                }
            }
        }
    }
    std::fs::write(path, bytes).unwrap();
}

fn fixture(root: &Path) -> std::path::PathBuf {
    write_ngr(&root.join("base.ngr"), 12, 131, 4);
    write_ngr(&root.join("map.ngr"), 6, 65, 2);
    let path = root.join("slide.vmu");
    std::fs::write(&path, "[Uncompressed Virtual Microscope Specimen]\nNoLayers=1\nBitsPerPixel=36\nPixelOrder=RGB\nImageFile(0,0,0)=base.ngr\nMapFile=map.ngr\nPhysicalWidth=6000\nPhysicalHeight=65500\nSourceLens=20\n").unwrap();
    path
}

#[test]
fn vmu_preserves_native_samples_across_columns_and_tile_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let slide = Slide::open(&path).expect("VMU must be detected and opened");
    for (level, width, height) in [(0, 12, 131), (1, 6, 65)] {
        let req = RegionRequest::new(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(level),
            (0, 0),
            (width, height),
        );
        let tile = slide.read_region(&req).unwrap();
        let CpuTileData::U16(actual) = tile.data() else {
            panic!("VMU must preserve its 12-bit samples in U16")
        };
        let expected: Vec<_> = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| (0..3).map(move |c| sample(x, y, c))))
            .collect();
        assert_eq!(actual.as_slice(), expected);
    }
}

#[test]
fn vmu_rejects_invalid_ngr_geometry_and_truncated_data() {
    for (offset, value) in [
        (0, 0),
        (4, 0),
        (8, 0),
        (12, 0),
        (12, 5),
        (24, 4),
        (24, u32::MAX),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path());
        let base = dir.path().join("base.ngr");
        let mut bytes = std::fs::read(&base).unwrap();
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        std::fs::write(base, bytes).unwrap();
        assert!(Slide::open(path).is_err(), "offset {offset}, value {value}");
    }
    for length in [0, 27, 41] {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path());
        let base = dir.path().join("base.ngr");
        let file = std::fs::OpenOptions::new().write(true).open(base).unwrap();
        file.set_len(length).unwrap();
        assert!(Slide::open(path).is_err());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let base = dir.path().join("base.ngr");
    let mut bytes = std::fs::read(&base).unwrap();
    for (offset, value) in [(4, u32::MAX), (8, u32::MAX), (12, 1)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    std::fs::write(base, bytes).unwrap();
    let error = Slide::open(path).unwrap_err();
    assert!(error.to_string().contains("overflow"), "{error}");
}

#[cfg(feature = "parity-openslide")]
#[test]
#[ignore = "requires an installed independent OpenSlide library"]
fn vmu_matches_openslide_geometry_metadata_and_rgb12_rendering() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let reference = wsi_rs_test_support::openslide::OpenSlide::open(&path).unwrap();
    let slide = Slide::open(&path).unwrap();
    let series = &slide.dataset().scenes[0].series[0];
    assert_eq!(reference.level_count(), 2);
    let reference_levels = reference.levels().unwrap();
    for (actual, expected) in series.levels.iter().zip(&reference_levels) {
        assert_eq!(actual.downsample, expected.downsample);
    }
    for key in [
        "openslide.vendor",
        "openslide.quickhash-1",
        "openslide.mpp-x",
        "openslide.mpp-y",
        "openslide.objective-power",
    ] {
        assert_eq!(
            slide.dataset().properties.get(key).map(str::to_owned),
            reference.property(key),
            "{key}"
        );
    }
    for (level, width, height) in [(0, 12, 131), (1, 6, 65)] {
        assert_eq!(
            series.levels[level as usize].dimensions,
            reference.level_dimensions(level)
        );
        let req = RegionRequest::new(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(level),
            (0, 0),
            (width, height),
        );
        let tile = slide.read_region(&req).unwrap();
        let CpuTileData::U16(values) = tile.data() else {
            panic!("expected RGB16")
        };
        let rgba: Vec<_> = values
            .chunks_exact(3)
            .flat_map(|p| [(p[0] >> 4) as u8, (p[1] >> 4) as u8, (p[2] >> 4) as u8, 255])
            .collect();
        assert_eq!(
            rgba,
            reference.read_region(0, 0, level, width, height).unwrap()
        );
    }
}

#[test]
fn vmu_virtual_tiles_cross_wide_storage_columns_and_enforce_limits() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    write_ngr(&dir.path().join("base.ngr"), 600, 131, 300);
    let slide = Slide::open(&path).unwrap();
    let req = wsi_rs::TileRequest::new(0usize, 0usize, 0, 1, 1);
    let tile = slide.read_tile(&req).unwrap();
    assert_eq!((tile.width(), tile.height()), (256, 64));
    let CpuTileData::U16(actual) = tile.data() else {
        panic!("expected U16")
    };
    let expected: Vec<_> = (64..128)
        .flat_map(|y| (256..512).flat_map(move |x| (0..3).map(move |c| sample(x, y, c))))
        .collect();
    assert_eq!(actual.as_slice(), expected);
    let limits = wsi_rs::SlideLimits::default()
        .with_encoded_unit_bytes(100)
        .unwrap();
    let limited = Slide::open_with_options(
        &path,
        wsi_rs::SlideOpenOptions::deterministic().with_limits(limits),
    )
    .unwrap();
    assert!(matches!(
        limited.read_tile(&req),
        Err(wsi_rs::WsiError::ResourceLimit { .. })
    ));
    for req in [
        wsi_rs::TileRequest::new(0usize, 0usize, 0, -1, 0),
        wsi_rs::TileRequest::new(0usize, 0usize, 0, 3, 0),
        wsi_rs::TileRequest::new(0usize, 0usize, 2, 0, 0),
    ] {
        assert!(slide.read_tile(&req).is_err());
    }
}

#[test]
fn vmu_rejects_unsupported_pixel_contracts_and_ambiguous_base_images() {
    for replacement in [
        "BitsPerPixel=24",
        "PixelOrder=BGR",
        "ImageFile(0,0)=base.ngr\nImageFile=base.ngr",
        "ImageFile(1,0)=base.ngr",
        "ImageFile=../outside.ngr",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path());
        let original = std::fs::read_to_string(&path).unwrap();
        let key = if replacement.starts_with("Bits") {
            "BitsPerPixel=36"
        } else if replacement.starts_with("Pixel") {
            "PixelOrder=RGB"
        } else {
            "ImageFile(0,0,0)=base.ngr"
        };
        std::fs::write(&path, original.replace(key, replacement)).unwrap();
        assert!(Slide::open(&path).is_err(), "{replacement}");
    }
}

#[test]
#[ignore = "requires WSI_RS_VMU_SHIM_LIBRARY pointing to the freshly built OpenSlide shim"]
fn vmu_openslide_abi_renders_native_rgb12() {
    let library = std::env::var_os("WSI_RS_VMU_SHIM_LIBRARY").expect("set shim library path");
    let api = wsi_rs_test_support::openslide::OpenSlideApi::load(Path::new(&library)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let shim = api.open(&path).unwrap();
    let reference = wsi_rs_test_support::openslide::OpenSlide::open(&path).unwrap();
    for (x, y, level, w, h) in [
        (0, 0, 0, 12, 131),
        (3, 63, 0, 7, 66),
        (-2, -2, 0, 16, 135),
        (0, 0, 1, 6, 65),
        (3, 127, 1, 4, 4),
    ] {
        assert_eq!(
            shim.read_region(x, y, level, w, h).unwrap(),
            reference.read_region(x, y, level, w, h).unwrap(),
            "region {x},{y} level {level}"
        );
    }
}

#[test]
fn vmu_macro_and_parallel_reads_use_bound_sources() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let macro_path = dir.path().join("macro.jpg");
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 95)
        .encode(
            &[120, 30, 240].repeat(12),
            4,
            3,
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
    std::fs::write(&macro_path, &jpeg).unwrap();
    let ini = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("{ini}MacroImage=macro.jpg\nImageFile(1,0,0)=unused-focal-plane.ngr\n"),
    )
    .unwrap();
    let slide = Slide::open(&path).unwrap();
    assert_eq!(
        slide.dataset().associated_images["macro"].dimensions,
        (4, 3)
    );
    let expected = jpeg_decoder::Decoder::new(std::io::Cursor::new(&jpeg))
        .decode()
        .unwrap();
    let actual = slide.read_associated("macro").unwrap().to_rgb().unwrap();
    assert!(actual
        .as_raw()
        .iter()
        .zip(&expected)
        .all(|(a, b)| a.abs_diff(*b) <= 3));
    // Replacing a path must not silently change the already-open macro source.
    std::fs::write(&macro_path, b"replacement").unwrap();
    assert_eq!(
        slide.read_associated("macro").unwrap().to_rgb().unwrap(),
        actual
    );
    std::thread::scope(|scope| {
        for col in 0..3 {
            let slide = &slide;
            scope.spawn(move || {
                for row in 0..3 {
                    let req = wsi_rs::TileRequest::new(0usize, 0usize, 0, col, row);
                    let tile = slide.read_tile(&req).unwrap();
                    let CpuTileData::U16(values) = tile.data() else {
                        panic!("expected U16")
                    };
                    for y in 0..tile.height() {
                        for x in 0..tile.width() {
                            for c in 0..3 {
                                assert_eq!(
                                    values[((y * tile.width() + x) * 3 + c) as usize],
                                    sample(col as u32 * 4 + x, row as u32 * 64 + y, c)
                                );
                            }
                        }
                    }
                }
            });
        }
    });
    let cancellation = wsi_rs::ReadCancellationToken::new();
    cancellation.cancel();
    let control = wsi_rs::ReadControl::new(cancellation);
    assert!(slide
        .read_tile_controlled(&wsi_rs::TileRequest::new(0usize, 0usize, 0, 0, 0), &control)
        .is_err());
}
