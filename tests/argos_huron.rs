mod support;

use support::corpus::{find_slide_by_alias, load_public};
use wsi_rs::{CpuTileData, PlaneSelection, Slide, TileLayout, TileRequest};

#[test]
#[ignore = "requires the public ARGOS/Huron corpus"]
fn public_argos_slides_expose_sparse_pyramids_z_planes_and_associated_images() {
    let manifest = load_public().expect("load public corpus manifest");
    let entries = manifest
        .slides
        .iter()
        .filter(|entry| entry.format == "argos")
        .collect::<Vec<_>>();
    assert!(!entries.is_empty(), "public corpus contains no ARGOS rows");
    for entry in entries {
        let alias = entry.alias.as_str();
        let path = find_slide_by_alias(alias)
            .unwrap_or_else(|| panic!("public corpus slide {alias} is missing"));
        assert_eq!(entry.format, "argos");

        let slide = Slide::open(&path).unwrap_or_else(|error| panic!("open {alias}: {error}"));
        assert_eq!(slide.dataset().properties.vendor(), Some("argos"));
        assert!(slide
            .dataset()
            .properties
            .get("openslide.barcode")
            .is_some());
        let series = &slide.dataset().scenes[0].series[0];
        assert!(series.levels.len() >= 3, "{alias} should expose a pyramid");
        if alias == "argos-001" {
            assert_eq!(series.axes.z, 1);
        } else {
            assert!(series.axes.z > 1, "stacked ARGOS must expose every Z plane");
        }

        let TileLayout::Irregular { tiles, .. } = &series.levels[0].tile_layout else {
            panic!("{alias} should model sparse source tiles explicitly");
        };
        let (&(col, row), _) = tiles
            .iter()
            .next()
            .unwrap_or_else(|| panic!("{alias} base level contains no source tiles"));
        let tile = slide
            .read_tile(
                &TileRequest::new(0usize, 0usize, 0u32, col, row)
                    .with_plane(PlaneSelection::default()),
            )
            .unwrap_or_else(|error| panic!("read {alias} source tile: {error}"));
        assert!(matches!(tile.data(), CpuTileData::U8(_)));

        for name in ["thumbnail", "macro"] {
            assert!(
                slide.dataset().associated_images.contains_key(name),
                "{alias} missing {name} metadata"
            );
            let image = slide
                .read_associated(name)
                .unwrap_or_else(|error| panic!("read {alias} {name}: {error}"));
            assert!(image.width() > 0 && image.height() > 0);
        }
    }
}

#[test]
#[ignore = "requires the public ARGOS/Huron corpus"]
fn public_huron_jpeg_and_uncompressed_slides_decode_with_associated_images() {
    let manifest = load_public().expect("load public corpus manifest");
    let entries = manifest
        .slides
        .iter()
        .filter(|entry| entry.format == "huron")
        .collect::<Vec<_>>();
    assert!(!entries.is_empty(), "public corpus contains no Huron rows");
    for entry in entries {
        let alias = entry.alias.as_str();
        let path = find_slide_by_alias(alias)
            .unwrap_or_else(|| panic!("public corpus slide {alias} is missing"));
        assert_eq!(entry.format, "huron");

        let slide = Slide::open(&path).unwrap_or_else(|error| panic!("open {alias}: {error}"));
        assert_eq!(slide.dataset().properties.vendor(), Some("huron"));
        let series = &slide.dataset().scenes[0].series[0];
        assert!(series.levels.len() >= 3, "{alias} should expose a pyramid");

        let tile = slide
            .read_tile(&TileRequest::new(0usize, 0usize, 0u32, 0, 0))
            .unwrap_or_else(|error| panic!("read {alias} tile: {error}"));
        assert!(matches!(tile.data(), CpuTileData::U8(_)));

        for name in ["thumbnail", "label", "macro"] {
            assert!(
                slide.dataset().associated_images.contains_key(name),
                "{alias} missing {name} metadata"
            );
            let image = slide
                .read_associated(name)
                .unwrap_or_else(|error| panic!("read {alias} {name}: {error}"));
            assert!(image.width() > 0 && image.height() > 0);
        }
    }
}
