use std::fs;

use tempfile::TempDir;

use super::*;

const RGB_CODESTREAM: &[u8] = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");

struct RawJp2kFixture {
    _temp: TempDir,
    path: std::path::PathBuf,
}

fn write_fixture(extension: &str, bytes: &[u8]) -> RawJp2kFixture {
    let temp = tempfile::tempdir().expect("temporary raw JP2K directory");
    let path = temp.path().join(format!("synthetic.{extension}"));
    fs::write(&path, bytes).expect("write raw JP2K fixture");
    RawJp2kFixture { _temp: temp, path }
}

fn expect_error<T>(result: Result<T, WsiError>, context: &str) -> WsiError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

#[test]
fn probe_requires_supported_extension_and_soc_marker() {
    let backend = RawJp2kBackend;
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let detected = backend
        .probe(&fixture.path)
        .expect("probe valid codestream");
    assert!(detected.detected);
    assert_eq!(detected.vendor, "raw-jp2k");
    assert_eq!(detected.confidence, ProbeConfidence::Definite);

    let upper = fixture.path.with_extension("J2C");
    fs::rename(&fixture.path, &upper).expect("rename to uppercase J2C");
    assert!(backend.probe(&upper).expect("probe uppercase J2C").detected);

    for path in [upper.with_extension("jp2"), upper.with_extension("")] {
        fs::write(&path, RGB_CODESTREAM).expect("write unsupported extension fixture");
        assert!(
            !backend
                .probe(&path)
                .expect("probe unsupported path")
                .detected
        );
    }

    let bad_magic = write_fixture("j2k", b"not a codestream");
    assert!(
        !backend
            .probe(&bad_magic.path)
            .expect("probe bad marker")
            .detected
    );
    assert!(
        !backend
            .probe(&bad_magic.path.with_file_name("missing.j2k"))
            .expect("probe missing file")
            .detected
    );
}

#[test]
fn open_exposes_codestream_resolution_levels_and_decodes_the_codestream() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open(&fixture.path)
        .expect("open raw JP2K fixture");
    let dataset = reader.dataset();

    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.scenes[0].name.as_deref(), Some("synthetic.j2k"));
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::default());
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert_eq!(series.channels.len(), 3);
    assert_eq!(series.channels[0].name.as_deref(), Some("R"));
    assert_eq!(series.channels[1].color, Some([0, 255, 0]));
    assert_eq!(series.channels[2].name.as_deref(), Some("B"));
    // Two wavelet decompositions yield two reduced levels that round up.
    let levels = series
        .levels
        .iter()
        .map(|level| (level.dimensions, level.downsample))
        .collect::<Vec<_>>();
    assert_eq!(levels, [((16, 12), 1.0), ((8, 6), 2.0), ((4, 3), 4.0)]);
    for level in &series.levels {
        assert!(matches!(
            level.tile_layout,
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across: 1,
                tiles_down: 1,
            } if (u64::from(tile_width), u64::from(tile_height)) == level.dimensions
        ));
    }
    assert!(dataset.associated_images.is_empty());
    assert!(dataset.icc_profiles.is_empty());
    assert_eq!(dataset.properties.vendor(), Some("raw-jp2k"));
    let quickhash = dataset
        .properties
        .quickhash1()
        .expect("raw JP2K quickhash property");
    assert_eq!(format!("{:032x}", dataset.id.get()), quickhash[..32]);

    let request = TileRequest::new(0, 0, 0, 0, 0);
    assert_eq!(reader.tile_codec_kind(&request), TileCodecKind::Jp2k);
    let tile = reader
        .read_tile_cpu(&request)
        .expect("decode raw JP2K tile");
    assert_eq!((tile.width(), tile.height()), (16, 12));
    assert_eq!(tile.channels(), 3);
    assert_eq!(tile.data.sample_type(), SampleType::Uint8);
    assert_eq!(tile.as_u8().expect("u8 JP2K pixels").len(), 16 * 12 * 3);

    let batch = reader
        .read_tiles_cpu(&[request.clone(), request.clone()])
        .expect("decode ordered raw JP2K batch");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].as_u8(), batch[1].as_u8());
}

#[test]
fn raw_tile_preserves_the_original_codestream_and_metadata() {
    let fixture = write_fixture("j2c", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open(&fixture.path)
        .expect("open raw JP2K fixture");
    let request = TileRequest::new(0, 0, 0, 0, 0);
    let raw = reader
        .read_raw_compressed_tile(&request)
        .expect("read raw compressed JP2K tile");

    assert_eq!(raw.compression(), Compression::Jp2kRgb);
    assert_eq!((raw.width(), raw.height()), (16, 12));
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!(raw.data(), RGB_CODESTREAM);

    let other_path = write_fixture("j2k", RGB_CODESTREAM);
    let other = RawJp2kBackend
        .open(&other_path.path)
        .expect("open same bytes at another path");
    assert_ne!(reader.dataset().id, other.dataset().id);
}

fn load_ppm(bytes: &[u8]) -> image::RgbImage {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Pnm)
        .expect("decode PPM reference")
        .to_rgb8()
}

/// A named codestream with its OpenJPEG reduction 1 and 2 references.
type ReducedReferenceCase = (&'static str, &'static [u8], [&'static [u8]; 2]);

#[test]
fn reduced_levels_match_openjpeg_reduced_decodes_exactly() {
    // References are OpenJPEG `-r 1`/`-r 2` decodes of the same codestreams,
    // covering irreversible 9/7 with and without MCT, reversible RCT, and a
    // multi-tile HTJ2K codestream with odd dimensions.
    let cases: [ReducedReferenceCase; 4] = [
        (
            "rgb_nomct",
            RGB_CODESTREAM,
            [
                include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.r2.ppm"),
            ],
        ),
        (
            "rgb_mct",
            include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.j2k"),
            [
                include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.r2.ppm"),
            ],
        ),
        (
            "rgb_rct",
            include_bytes!("../../../tests/fixtures/jp2k/rgb_rct.j2k"),
            [
                include_bytes!("../../../tests/fixtures/jp2k/rgb_rct.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/rgb_rct.r2.ppm"),
            ],
        ),
        (
            "openjph_rgb_u8_53",
            include_bytes!("../../../tests/fixtures/jp2k/openjph_rgb_u8_53.j2k"),
            [
                include_bytes!("../../../tests/fixtures/jp2k/openjph_rgb_u8_53.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/openjph_rgb_u8_53.r2.ppm"),
            ],
        ),
    ];
    for (name, codestream, references) in cases {
        let fixture = write_fixture("j2k", codestream);
        let reader = RawJp2kBackend.open(&fixture.path).expect(name);
        let levels = &reader.dataset().scenes[0].series[0].levels;
        assert_eq!(levels.len(), 3, "{name}");
        for (level, reference) in (1u32..).zip(references) {
            let expected = load_ppm(reference);
            let dimensions = (u64::from(expected.width()), u64::from(expected.height()));
            assert_eq!(
                levels[level as usize].dimensions, dimensions,
                "{name} {level}"
            );
            let tile = reader
                .read_tile_cpu(&TileRequest::new(0, 0, level, 0, 0))
                .expect(name);
            assert_eq!(
                (tile.width(), tile.height()),
                expected.dimensions(),
                "{name} level {level}"
            );
            assert_eq!(
                tile.as_u8(),
                Some(expected.as_raw().as_slice()),
                "{name} level {level}"
            );
        }
    }
}

#[test]
fn exposed_levels_follow_the_codestream_decomposition_count() {
    let pixels = (0..8 * 8 * 3)
        .map(|index| (index * 7) as u8)
        .collect::<Vec<_>>();
    for (decompositions, expected) in [(0, vec![(8, 8)]), (3, vec![(8, 8), (4, 4), (2, 2), (1, 1)])]
    {
        let options = j2k_native::EncodeOptions {
            reversible: true,
            num_decomposition_levels: decompositions,
            ..j2k_native::EncodeOptions::default()
        };
        let codestream =
            j2k_native::encode(&pixels, 8, 8, 3, 8, false, &options).expect("encode fixture");
        let fixture = write_fixture("j2c", &codestream);
        let reader = RawJp2kBackend.open(&fixture.path).unwrap();
        let dimensions = reader.dataset().scenes[0].series[0]
            .levels
            .iter()
            .map(|level| level.dimensions)
            .collect::<Vec<_>>();
        assert_eq!(dimensions, expected, "{decompositions} decompositions");
        let smallest = u32::try_from(expected.len() - 1).unwrap();
        let tile = reader
            .read_tile_cpu(&TileRequest::new(0, 0, smallest, 0, 0))
            .unwrap();
        let (width, height) = expected[expected.len() - 1];
        assert_eq!(
            (u64::from(tile.width()), u64::from(tile.height())),
            (width, height)
        );
    }
}

#[test]
fn reduced_levels_decode_on_cpu_only_and_have_no_passthrough() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let managed = RawJp2kBackend
        .open_with_config(&fixture.path, BackendOpenConfig::deterministic())
        .unwrap();
    let reduced = TileRequest::new(0, 0, 1, 0, 0);
    assert_eq!(managed.tile_codec_kind(&reduced), TileCodecKind::Jp2k);
    assert!(matches!(
        managed.read_raw_compressed_tile(&reduced),
        Err(WsiError::Unsupported { .. })
    ));
    #[cfg(any(feature = "metal", feature = "cuda"))]
    assert!(managed
        .prepare_adaptive_jp2k(std::slice::from_ref(&reduced), 2, None)
        .is_none());
    #[cfg(feature = "metal")]
    if let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() {
        assert!(matches!(
            managed.read_tiles_metal(std::slice::from_ref(&reduced), &sessions),
            Err(WsiError::Unsupported { .. })
        ));
    }
    let slide = crate::Slide::open(&fixture.path).unwrap();
    let tile = slide.read_tile(&reduced).unwrap();
    assert_eq!((tile.width(), tile.height()), (8, 6));
}

#[test]
fn request_validation_rejects_non_default_axes_and_tiles() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open(&fixture.path)
        .expect("open raw JP2K fixture");

    for request in [
        TileRequest::new(1, 0, 0, 0, 0),
        TileRequest::new(0, 1, 0, 0, 0),
        TileRequest::new(0, 0, 3, 0, 0),
        TileRequest::new(0, 0, 0, -1, 0),
        TileRequest::new(0, 0, 0, 0, 1),
    ] {
        assert!(matches!(
            reader.read_tile_cpu(&request),
            Err(WsiError::TileRead { .. })
        ));
        assert!(matches!(
            reader.read_raw_compressed_tile(&request),
            Err(WsiError::TileRead { .. })
        ));
    }

    let non_default_plane =
        TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection { z: 1, c: 0, t: 0 });
    assert!(matches!(
        reader.read_tile_cpu(&non_default_plane),
        Err(WsiError::Unsupported { .. })
    ));
}

#[test]
fn open_reports_path_context_and_rejects_truncated_or_unsupported_codestreams() {
    let missing_dir = tempfile::tempdir().expect("temporary missing-file directory");
    let missing_path = missing_dir.path().join("missing.j2k");
    let error = expect_error(
        RawJp2kBackend.open(&missing_path),
        "opening a missing JP2K file must fail",
    );
    assert!(matches!(error, WsiError::IoWithPath { path, .. } if path == missing_path));

    for bytes in [&b"\xff\x4f"[..], &b"\xff\x4f\xff\x51\0\x02"[..]] {
        let fixture = write_fixture("j2k", bytes);
        assert!(RawJp2kBackend.open(&fixture.path).is_err());
    }
}

#[test]
fn configured_encoded_limit_rejects_before_reading_the_codestream() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let limits = crate::SlideLimits::default()
        .with_encoded_unit_bytes(1)
        .expect("nonzero encoded limit");
    let config = BackendOpenConfig::new(crate::CacheConfig::deterministic(), limits);
    let error = expect_error(
        RawJp2kBackend.open_with_config(&fixture.path, config),
        "tiny encoded limit must reject raw JP2K input",
    );
    assert!(matches!(error, WsiError::ResourceLimit { .. }));
}

#[test]
fn managed_reader_reports_encoded_working_set_bounds() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open_with_config(&fixture.path, BackendOpenConfig::deterministic())
        .expect("open managed raw JP2K fixture");
    let encoded_len = RGB_CODESTREAM.len() as u64;
    let tile = TileRequest::new(0, 0, 0, 0, 0);
    let display = TileViewRequest::new(0, 0, 0, 0, 0, 16, 12);
    let region = RegionRequest::new(0, 0, 0, (0, 0), (16, 12));

    assert_eq!(reader.tile_encoded_upper_bound(&tile).unwrap(), encoded_len);
    assert_eq!(reader.tile_batch_encoded_upper_bound(&[]).unwrap(), 0);
    assert_eq!(
        reader
            .tile_batch_encoded_upper_bound(&[tile.clone(), tile])
            .unwrap(),
        encoded_len
    );
    assert_eq!(
        reader.display_tile_encoded_upper_bound(&display).unwrap(),
        encoded_len
    );
    assert_eq!(reader.associated_encoded_upper_bound("label").unwrap(), 0);
    assert_eq!(
        reader.region_fastpath_encoded_upper_bound(&region).unwrap(),
        encoded_len
    );
}
