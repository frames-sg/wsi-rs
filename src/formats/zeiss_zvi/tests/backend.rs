use std::fs;

use super::super::*;
use super::fixtures::*;
use crate::core::registry::Slide;

fn read_plane(reader: &dyn SlideReader, plane: PlaneSelection, col: i64) -> CpuTile {
    reader
        .read_tile_cpu(&TileRequest::new(0, 0, 0, col, 0).with_plane(plane))
        .expect("read synthetic ZVI plane")
}

#[test]
fn probe_accepts_only_zvi_shaped_compound_files() {
    let fixture = ZviFixture::whole_u8();
    let detected = ZeissZviBackend
        .probe(&fixture.path)
        .expect("probe synthetic ZVI");
    assert!(detected.detected);
    assert_eq!(detected.vendor, "zeiss");
    assert_eq!(detected.confidence, ProbeConfidence::Definite);

    let absent = fixture.path.with_file_name("absent.zvi");
    assert!(!ZeissZviBackend.probe(&absent).unwrap().detected);

    let wrong_magic = fixture.path.with_file_name("wrong.zvi");
    fs::write(&wrong_magic, b"not a compound file").expect("write wrong magic fixture");
    assert!(!ZeissZviBackend.probe(&wrong_magic).unwrap().detected);

    let corrupt = fixture.path.with_file_name("corrupt.zvi");
    fs::write(&corrupt, CFB_MAGIC).expect("write corrupt compound fixture");
    assert!(!ZeissZviBackend.probe(&corrupt).unwrap().detected);

    let empty = empty_compound();
    assert!(!ZeissZviBackend.probe(&empty.path).unwrap().detected);
}

#[test]
fn configured_metadata_limit_rejects_zvi_tag_streams() {
    let fixture = ZviFixture::whole_u8();
    let limits = crate::SlideLimits::default()
        .with_metadata_value_bytes(1)
        .expect("nonzero metadata limit");
    let config = BackendOpenConfig::new(crate::CacheConfig::deterministic(), limits);
    let error = match ZeissZviBackend.open_with_config(&fixture.path, config) {
        Ok(_) => panic!("tiny configured metadata limit must reject ZVI tags"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        WsiError::ResourceLimit {
            resource: "individual metadata value",
            ..
        }
    ));
}

#[test]
fn configured_index_limit_rejects_zvi_stream_table() {
    let fixture = ZviFixture::whole_u8();
    let limits = crate::SlideLimits::default()
        .with_tile_index_bytes(1)
        .expect("nonzero index limit");
    let config = BackendOpenConfig::new(crate::CacheConfig::deterministic(), limits);
    let error = match ZeissZviBackend.open_with_config(&fixture.path, config) {
        Ok(_) => panic!("tiny configured index limit must reject ZVI stream indexes"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        WsiError::ResourceLimit {
            resource: "tile/frame index",
            ..
        }
    ));
}

#[test]
fn whole_image_opens_with_metadata_channels_and_associated_thumbnail() {
    let fixture = ZviFixture::whole_u8();
    let reader = ZeissZviBackend
        .open(&fixture.path)
        .expect("open synthetic ZVI");
    let dataset = reader.dataset();

    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.scenes[0].name.as_deref(), Some("Image"));
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape { z: 1, c: 3, t: 1 });
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert_eq!(series.levels[0].dimensions, (8, 4));
    assert!(matches!(
        series.levels[0].tile_layout,
        TileLayout::WholeLevel {
            width: 8,
            height: 4,
            ..
        }
    ));
    assert_eq!(series.channels[0].name.as_deref(), Some("Raw"));
    assert_eq!(series.channels[1].color, Some([0x44, 0x55, 0x66]));
    assert_eq!(series.channels[2].name.as_deref(), Some("JPEG"));
    assert_eq!(dataset.properties.vendor(), Some("zeiss"));
    assert_eq!(dataset.properties.get("zeiss.image.size_c"), Some("3"));
    assert_eq!(dataset.properties.get("openslide.mpp-x"), Some("0.500000"));
    assert_eq!(
        dataset.properties.get("zeiss.objective.name"),
        Some("Plan-Apochromat 20x")
    );
    assert_eq!(
        dataset.properties.get("openslide.objective-power"),
        Some("20")
    );
    let quickhash = dataset.properties.quickhash1().expect("ZVI quickhash");
    assert_eq!(format!("{:032x}", dataset.id.get()), quickhash[..32]);

    assert_eq!(dataset.associated_images["thumbnail"].dimensions, (2, 1));
    let thumbnail = reader
        .read_associated("thumbnail")
        .expect("read ZVI thumbnail");
    assert_eq!((thumbnail.width(), thumbnail.height()), (2, 1));
    assert_eq!(thumbnail.channels(), 3);
    assert!(matches!(
        reader.read_associated("missing"),
        Err(WsiError::AssociatedImageNotFound(name)) if name == "missing"
    ));
}

#[test]
fn raw_zlib_and_jpeg_planes_decode_through_the_reader_boundary() {
    let fixture = ZviFixture::whole_u8();
    let reader = ZeissZviBackend
        .open(&fixture.path)
        .expect("open synthetic ZVI");

    let raw = read_plane(reader.as_ref(), PlaneSelection::new(0, 0, 0), 0);
    assert_eq!((raw.width(), raw.height()), (8, 4));
    assert_eq!(raw.color_space(), &ColorSpace::Grayscale);
    assert_eq!(raw.as_u8().unwrap(), (0u8..32).collect::<Vec<_>>());

    let inflated = read_plane(reader.as_ref(), PlaneSelection::new(0, 1, 0), 0);
    assert_eq!(inflated.as_u8().unwrap(), (100u8..132).collect::<Vec<_>>());

    let jpeg = read_plane(reader.as_ref(), PlaneSelection::new(0, 2, 0), 0);
    assert_eq!((jpeg.width(), jpeg.height()), (8, 4));
    assert_eq!(jpeg.channels(), 3);
    assert_eq!(jpeg.as_u8().unwrap().len(), 96);
    assert!(jpeg.as_u8().unwrap().iter().all(|sample| *sample >= 190));
}

#[test]
fn u16_whole_level_reads_full_and_edge_windows_without_byte_swapping() {
    let fixture = ZviFixture::raw_u16();
    let reader = ZeissZviBackend.open(&fixture.path).expect("open U16 ZVI");
    assert_eq!(
        reader.dataset().scenes[0].series[0].sample_type,
        SampleType::Uint16
    );

    let first = read_plane(reader.as_ref(), PlaneSelection::default(), 0);
    assert_eq!((first.width(), first.height()), (256, 2));
    assert_eq!(first.data.as_u16().unwrap()[0], 0);
    assert_eq!(first.data.as_u16().unwrap()[255], 765);
    assert_eq!(first.data.as_u16().unwrap()[256], 780);

    let edge = read_plane(reader.as_ref(), PlaneSelection::default(), 1);
    assert_eq!((edge.width(), edge.height()), (4, 2));
    assert_eq!(
        edge.data.as_u16().unwrap(),
        &[768, 771, 774, 777, 1548, 1551, 1554, 1557]
    );
}

#[test]
fn default_batch_and_region_composition_preserve_pixels_and_zero_fill() {
    let fixture = ZviFixture::whole_u8();
    let reader = ZeissZviBackend
        .open(&fixture.path)
        .expect("open synthetic ZVI");
    let requests = [
        TileRequest::new(0, 0, 0, 0, 0),
        TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection::new(0, 1, 0)),
    ];
    let batch = reader
        .read_tiles_cpu(&requests)
        .expect("read ordered ZVI batch");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].as_u8().unwrap()[0], 0);
    assert_eq!(batch[1].as_u8().unwrap()[0], 100);

    let slide = Slide::from_source_with_cache_bytes(reader, 1 << 20);
    let region = slide
        .read_region(&RegionRequest::new(0, 0, 0, (-2, 1), (6, 2)))
        .expect("compose partly out-of-bounds ZVI region");
    assert_eq!((region.width(), region.height()), (6, 2));
    assert_eq!(
        region.as_u8().unwrap(),
        &[0, 0, 8, 9, 10, 11, 0, 0, 16, 17, 18, 19]
    );
}

#[test]
fn mosaic_maps_stage_positions_to_tiles_and_composes_across_them() {
    let fixture = ZviFixture::mosaic();
    let reader = ZeissZviBackend
        .open(&fixture.path)
        .expect("open mosaic ZVI");
    let level = &reader.dataset().scenes[0].series[0].levels[0];
    assert_eq!(level.dimensions, (512, 2));
    assert!(matches!(
        &level.tile_layout,
        TileLayout::Irregular { tiles, .. } if tiles.len() == 2
    ));
    assert!(read_plane(reader.as_ref(), PlaneSelection::default(), 0)
        .as_u8()
        .unwrap()
        .iter()
        .all(|sample| *sample == 17));
    assert!(read_plane(reader.as_ref(), PlaneSelection::default(), 1)
        .as_u8()
        .unwrap()
        .iter()
        .all(|sample| *sample == 231));

    let slide = Slide::from_source_with_cache_bytes(reader, 1 << 20);
    let region = slide
        .read_region(&RegionRequest::new(0, 0, 0, (254, 0), (4, 1)))
        .expect("compose across ZVI mosaic tiles");
    assert_eq!(region.as_u8().unwrap(), &[17, 17, 231, 231]);
}
