use std::io::Write;

use wsi_rs::{Slide, WsiError};

#[test]
fn builtin_registry_rejects_truncated_czi() {
    let mut file = tempfile::Builder::new()
        .suffix(".czi")
        .tempfile()
        .expect("create CZI-shaped input");
    file.write_all(b"ZISRAWFILE\0\0\0\0\0\0")
        .expect("write CZI magic");

    let error = Slide::open(file.path()).expect_err("truncated CZI must not open");
    assert!(
        matches!(error, WsiError::InvalidSlide { .. }),
        "unexpected CZI validation error: {error}"
    );
}

#[test]
#[ignore = "requires WSI_RS_CZI_JXR_PATH pointing to the public Zeiss-5-JXR.czi corpus"]
fn real_jpegxr_czi_reads_tiles_regions_and_associated_images() {
    let path = std::env::var_os("WSI_RS_CZI_JXR_PATH").expect("WSI_RS_CZI_JXR_PATH");
    let slide = Slide::open(path).expect("open real JPEG XR CZI");
    assert_eq!(slide.dataset().properties.vendor(), Some("zeiss"));
    let levels = &slide.dataset().scenes[0].series[0].levels;
    assert!(levels.len() > 1);
    let mut nonzero = 0;
    for (index, level) in levels.iter().enumerate() {
        // Sample the first acquired region; the canvas center lies between scenes.
        let region = |field: &str| -> f64 {
            slide
                .dataset()
                .properties
                .get(&format!("openslide.region[0].{field}"))
                .expect("native CZI acquired-region metadata")
                .parse()
                .unwrap()
        };
        let x = ((region("x") + region("width") / 2.0) / level.downsample) as u64;
        let y = ((region("y") + region("height") / 2.0) / level.downsample) as u64;
        let request = wsi_rs::TileRequest::new(
            0usize,
            0usize,
            index as u32,
            (x / 256) as i64,
            (y / 256) as i64,
        );
        let tile = slide.read_tile(&request).expect("real CZI tile");
        nonzero += tile
            .data()
            .as_u8()
            .unwrap()
            .iter()
            .filter(|&&v| v != 0)
            .count();
        let repeated = slide
            .read_tiles(&[request.clone(), request])
            .expect("ordered CZI batch");
        assert_eq!(repeated.len(), 2);
        assert_eq!(repeated[0].data().as_u8(), tile.data().as_u8());
        assert_eq!(repeated[1].data().as_u8(), tile.data().as_u8());
        let region = wsi_rs::RegionRequest::builder(0usize, 0usize, index as u32)
            .origin_px((x as i64, y as i64))
            .size_px((32, 32))
            .build()
            .unwrap();
        assert_eq!(
            slide.read_region_rgba(&region).unwrap().dimensions(),
            (32, 32)
        );
    }
    assert!(nonzero > 0, "sampled tissue must contain pixels");
    for (name, info) in &slide.dataset().associated_images {
        let image = slide
            .read_associated(name)
            .expect("real CZI associated image");
        assert_eq!((image.width(), image.height()), info.dimensions);
    }
}

#[test]
fn czi_fuzz_seed_reaches_pixel_decoding() {
    let seed = include_bytes!("fixtures/jxr/rgb.czi");
    let mut file = tempfile::Builder::new().suffix(".czi").tempfile().unwrap();
    file.write_all(seed).unwrap();
    let slide = Slide::open(file.path()).unwrap();
    let tile = slide
        .read_tile(&wsi_rs::TileRequest::new(0usize, 0usize, 0, 0, 0))
        .unwrap();
    let expected = image::load_from_memory(include_bytes!("fixtures/jxr/rgb.ppm"))
        .unwrap()
        .into_rgb8();
    assert_eq!(tile.data().as_u8(), Some(expected.as_raw().as_slice()));
}

#[test]
fn czi_jpegxr_tile_location_is_the_subblock_stream_and_round_trips() {
    let seed = include_bytes!("fixtures/jxr/rgb.czi");
    let mut file = tempfile::Builder::new().suffix(".czi").tempfile().unwrap();
    file.write_all(seed).unwrap();
    let slide = Slide::open(file.path()).unwrap();
    let req = wsi_rs::TileRequest::new(0usize, 0usize, 0, 0, 0);

    let wsi_rs::TileLocation::Stored(stored) = slide.tile_location(&req).unwrap() else {
        panic!("the CZI tile is stored")
    };
    let unit = stored.single_unit().expect("one subblock paints the tile");
    assert_eq!(unit.codec().compression(), wsi_rs::Compression::JpegXr);
    assert_eq!(unit.framing(), wsi_rs::StoredFraming::Standalone);
    let [segment] = unit.segments() else {
        panic!("a subblock data part is one byte range")
    };
    let bytes = &seed[segment.offset as usize..segment.end().unwrap() as usize];
    assert_eq!(bytes, include_bytes!("fixtures/jxr/rgb.jxr"));

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(unit.assemble([bytes]).unwrap(), raw.data());
    assert_eq!(raw.compression(), wsi_rs::Compression::JpegXr);

    let level = slide
        .level_tile_locations(0usize, 0usize, 0, wsi_rs::PlaneSelection::default())
        .unwrap();
    assert_eq!(level.tiles.len(), 1);
}
