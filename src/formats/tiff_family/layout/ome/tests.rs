use super::metadata::{DimensionOrder, PlaneShape};
use super::*;
use crate::core::registry::{DatasetReader, FormatProbe};
use crate::core::types::*;
use crate::formats::tiff_family::layout::generic::GenericTiffInterpreter;
use crate::formats::tiff_family::test_support::{build_tiff_tree, SyntheticIfd, SyntheticTag};
use crate::formats::tiff_family::TiffFamilyBackend;

const OME_NS: &str = "http://www.openmicroscopy.org/Schemas/OME/2016-06";

/// Uncompressed tiled plane whose every sample equals `fill` (truncated to
/// 8 bits for 8-bit planes).
fn plane(
    width: u32,
    height: u32,
    tile: u32,
    samples: u16,
    bits: u16,
    fill: u16,
) -> Vec<SyntheticTag> {
    let tiles = (width.div_ceil(tile) * height.div_ceil(tile)) as usize;
    let sample = &fill.to_le_bytes()[..usize::from(bits / 8)];
    let tile_len = (tile * tile) as usize * usize::from(samples) * sample.len();
    let tile_bytes = sample
        .iter()
        .copied()
        .cycle()
        .take(tile_len)
        .collect::<Vec<_>>();
    vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::short(tags::BITS_PER_SAMPLE, bits),
        SyntheticTag::short(tags::COMPRESSION, 1),
        SyntheticTag::short(tags::PHOTOMETRIC, if samples == 3 { 2 } else { 1 }),
        SyntheticTag::short(tags::SAMPLES_PER_PIXEL, samples),
        SyntheticTag::short(tags::PLANAR_CONFIGURATION, 1),
        SyntheticTag::long(tags::TILE_WIDTH, tile),
        SyntheticTag::long(tags::TILE_LENGTH, tile),
        SyntheticTag::blob_offsets(tags::TILE_OFFSETS, vec![tile_bytes.clone(); tiles]),
        SyntheticTag::long_array(tags::TILE_BYTE_COUNTS, &vec![tile_len as u32; tiles]),
    ]
}

fn with_description(mut plane_tags: Vec<SyntheticTag>, xml: &str) -> Vec<SyntheticTag> {
    plane_tags.push(SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, xml));
    plane_tags
}

fn ome(images: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><OME xmlns="{OME_NS}" UUID="urn:uuid:self" Creator="wsi-rs test">{images}</OME>"#
    )
}

/// The palom/tifffile brightfield layout: one interleaved RGB plane whose
/// pyramid is in SubIFDs.
fn brightfield_xml(physical_size: &str) -> String {
    ome(&format!(
        r#"<Image ID="Image:0" Name="Image0"><Pixels ID="Pixels:0" DimensionOrder="XYCZT" Type="uint8" SizeX="64" SizeY="48" SizeC="3" SizeZ="1" SizeT="1" Interleaved="true" {physical_size}><Channel ID="Channel:0:0" SamplesPerPixel="3"/><TiffData IFD="0" PlaneCount="1"/></Pixels></Image>"#
    ))
}

fn brightfield_file(physical_size: &str) -> tempfile::NamedTempFile {
    build_tiff_tree(&[SyntheticIfd {
        tags: with_description(plane(64, 48, 16, 3, 8, 10), &brightfield_xml(physical_size)),
        sub_ifds: vec![plane(32, 24, 16, 3, 8, 11), plane(16, 12, 16, 3, 8, 12)],
    }])
}

fn interpret_file(file: &tempfile::NamedTempFile) -> Result<DatasetLayout, TiffParseError> {
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(OmeTiffInterpreter.detect(&container));
    OmeTiffInterpreter.interpret(&container)
}

fn interpret_error(file: &tempfile::NamedTempFile) -> String {
    interpret_file(file).unwrap_err().to_string()
}

fn tile_request(level: u32, plane: PlaneSelection) -> TileRequest {
    TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: level.into(),
        plane: plane.into(),
        col: 0,
        row: 0,
    }
}

#[test]
fn ome_xml_is_claimed_by_ome_and_declined_by_generic() {
    let file = brightfield_file("");
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(OmeTiffInterpreter.detect(&container));
    assert!(!GenericTiffInterpreter.detect(&container));

    let plain = build_tiff_tree(&[SyntheticIfd {
        tags: plane(16, 16, 16, 3, 8, 0),
        sub_ifds: Vec::new(),
    }]);
    let container = TiffContainer::open(plain.path()).unwrap();
    assert!(!OmeTiffInterpreter.detect(&container));
    assert!(GenericTiffInterpreter.detect(&container));
}

#[test]
fn brightfield_pyramid_comes_from_sub_ifds() {
    let file = brightfield_file(
        r#"PhysicalSizeX="0.325" PhysicalSizeXUnit="µm" PhysicalSizeY="325" PhysicalSizeYUnit="nm""#,
    );
    let layout = interpret_file(&file).unwrap();

    assert_eq!(layout.dataset.scenes.len(), 1);
    let series = &layout.dataset.scenes[0].series[0];
    let dimensions = series
        .levels
        .iter()
        .map(|level| level.dimensions)
        .collect::<Vec<_>>();
    assert_eq!(dimensions, [(64, 48), (32, 24), (16, 12)]);
    let downsamples = series
        .levels
        .iter()
        .map(|level| level.downsample)
        .collect::<Vec<_>>();
    assert_eq!(downsamples, [1.0, 2.0, 4.0]);
    assert_eq!(series.axes, AxesShape::default());
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert!(series.channels.is_empty(), "interleaved RGB is brightfield");
    assert_eq!(layout.tile_sources.len(), 3);
    assert!(layout.associated_sources.is_empty());

    let properties = &layout.dataset.properties;
    assert_eq!(properties.vendor(), Some("ome-tiff"));
    assert_eq!(properties.get("openslide.mpp-x"), Some("0.325"));
    assert_eq!(properties.get("openslide.mpp-y"), Some("0.325"));
    assert!(properties.quickhash1().is_some());
    assert_eq!(properties.get("ome.Creator"), Some("wsi-rs test"));
    assert_eq!(properties.get("ome.image[0].Name"), Some("Image0"));
    assert_eq!(properties.get("ome.image[0].Pixels.SizeC"), Some("3"));
    assert_eq!(
        properties.get("ome.image[0].Pixels.Channel[0].SamplesPerPixel"),
        Some("3")
    );
    assert!(properties
        .get("openslide.comment")
        .is_some_and(|comment| comment.contains("<OME")));
}

#[test]
fn backend_reads_each_sub_ifd_level() {
    let file = brightfield_file("");
    let backend = TiffFamilyBackend::new();
    assert_eq!(backend.probe(file.path()).unwrap().vendor, "ome-tiff");
    let reader = backend.open(file.path()).unwrap();

    // Level 2 is 16x12, so its only tile is cropped to the level bounds.
    for (level, fill, height) in [(0u32, 10u8, 16), (1, 11, 16), (2, 12, 12)] {
        let tile = reader
            .read_tile_cpu(&tile_request(level, PlaneSelection::default()))
            .unwrap();
        assert_eq!((tile.width, tile.height, tile.channels), (16, height, 3));
        let samples = tile.data.as_u8().unwrap();
        assert!(
            samples.iter().all(|&sample| sample == fill),
            "level {level} must decode its own SubIFD"
        );
    }
}

#[test]
fn missing_physical_size_leaves_mpp_unset() {
    let layout = interpret_file(&brightfield_file(
        r#"PhysicalSizeX="1" PhysicalSizeXUnit="parsec""#,
    ))
    .unwrap();
    assert_eq!(layout.dataset.properties.get("openslide.mpp-x"), None);
    assert_eq!(layout.dataset.properties.get("openslide.mpp-y"), None);
    assert_eq!(
        layout
            .dataset
            .properties
            .get("ome.image[0].Pixels.PhysicalSizeXUnit"),
        Some("parsec")
    );
}

fn fluorescence_xml(tiff_data: &str) -> String {
    ome(&format!(
        r#"<Instrument ID="Instrument:0"><Objective ID="Objective:0" NominalMagnification="20"/></Instrument><Image ID="Image:0" Name="scan"><ObjectiveSettings ID="Objective:0"/><Pixels ID="Pixels:0" DimensionOrder="XYCZT" Type="uint16" SizeX="32" SizeY="32" SizeC="2" SizeZ="1" SizeT="1"><Channel ID="Channel:0:0" Name="DAPI" SamplesPerPixel="1" Color="-16776961" ExcitationWavelength="405"/><Channel ID="Channel:0:1" SamplesPerPixel="1" EmissionWavelength="0.45" EmissionWavelengthUnit="µm"/>{tiff_data}</Pixels></Image>"#
    ))
}

fn fluorescence_file(xml: &str, channel1_sub_ifds: usize) -> tempfile::NamedTempFile {
    build_tiff_tree(&[
        SyntheticIfd {
            tags: with_description(plane(32, 32, 16, 1, 16, 1000), xml),
            sub_ifds: vec![plane(16, 16, 16, 1, 16, 1001), plane(8, 8, 16, 1, 16, 1002)],
        },
        SyntheticIfd {
            tags: plane(32, 32, 16, 1, 16, 2000),
            sub_ifds: [plane(16, 16, 16, 1, 16, 2001), plane(8, 8, 16, 1, 16, 2002)]
                .into_iter()
                .take(channel1_sub_ifds)
                .collect(),
        },
    ])
}

#[test]
fn fluorescence_channels_map_to_planes_with_metadata() {
    let xml = fluorescence_xml(r#"<TiffData IFD="0" PlaneCount="2"/>"#);
    let file = fluorescence_file(&xml, 2);
    let layout = interpret_file(&file).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::new(1, 2, 1));
    assert_eq!(series.sample_type, SampleType::Uint16);
    assert_eq!(series.levels.len(), 3);
    assert_eq!(layout.tile_sources.len(), 6);
    assert_eq!(series.channels[0].name.as_deref(), Some("DAPI"));
    assert_eq!(series.channels[0].color, Some([255, 0, 0]));
    assert_eq!(series.channels[0].excitation_nm, Some(405.0));
    assert_eq!(series.channels[1].name.as_deref(), Some("Channel 1"));
    assert_eq!(series.channels[1].emission_nm, Some(450.0));
    assert_eq!(
        layout.dataset.properties.get("openslide.objective-power"),
        Some("20")
    );

    let reader = TiffFamilyBackend::new().open(file.path()).unwrap();
    for (c, level, fill) in [(0, 0, 1000u16), (1, 0, 2000), (1, 1, 2001), (0, 2, 1002)] {
        let tile = reader
            .read_tile_cpu(&tile_request(level, PlaneSelection::new(0, c, 0)))
            .unwrap();
        assert_eq!(tile.channels, 1);
        assert!(
            tile.data
                .as_u16()
                .unwrap()
                .iter()
                .all(|&sample| sample == fill),
            "channel {c} level {level} must decode its own IFD"
        );
    }
}

#[test]
fn unequal_channel_pyramids_expose_only_shared_levels() {
    let xml = fluorescence_xml(r#"<TiffData IFD="0" PlaneCount="2"/>"#);
    let layout = interpret_file(&fluorescence_file(&xml, 1)).unwrap();
    assert_eq!(layout.dataset.scenes[0].series[0].levels.len(), 2);
    assert_eq!(layout.tile_sources.len(), 4);
}

#[test]
fn unmapped_plane_is_rejected() {
    let xml = fluorescence_xml(r#"<TiffData IFD="0" PlaneCount="1"/>"#);
    let error = interpret_error(&fluorescence_file(&xml, 2));
    assert!(
        error.contains("plane (z=0, c=1, t=0) has no IFD"),
        "{error}"
    );
}

#[test]
fn tiff_data_first_coordinates_and_dimension_order_route_planes() {
    // XYZCT with 2 Z and 2 C: linear plane = z + 2c. IFDs are listed out of
    // order and each TiffData names its plane explicitly.
    let xml = ome(
        r#"<Image ID="Image:0"><Pixels ID="Pixels:0" DimensionOrder="XYZCT" Type="uint8" SizeX="16" SizeY="16" SizeC="2" SizeZ="2" SizeT="1"><Channel SamplesPerPixel="1"/><Channel SamplesPerPixel="1"/><TiffData IFD="0" FirstZ="1" FirstC="1"/><TiffData IFD="1" FirstZ="0" FirstC="0"/><TiffData IFD="2" FirstZ="0" FirstC="1"/><TiffData IFD="3" FirstZ="1" FirstC="0"/></Pixels></Image>"#,
    );
    let mut ifds = (0..4)
        .map(|index| SyntheticIfd {
            tags: plane(16, 16, 16, 1, 8, 50 + index),
            sub_ifds: Vec::new(),
        })
        .collect::<Vec<_>>();
    ifds[0]
        .tags
        .push(SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, &xml));
    let file = build_tiff_tree(&ifds);

    let reader = TiffFamilyBackend::new().open(file.path()).unwrap();
    assert_eq!(
        reader.dataset().scenes[0].series[0].axes,
        AxesShape::new(2, 2, 1)
    );
    for (z, c, fill) in [(1, 1, 50u8), (0, 0, 51), (0, 1, 52), (1, 0, 53)] {
        let tile = reader
            .read_tile_cpu(&tile_request(0, PlaneSelection::new(z, c, 0)))
            .unwrap();
        assert!(
            tile.data
                .as_u8()
                .unwrap()
                .iter()
                .all(|&sample| sample == fill),
            "(z={z}, c={c}) must decode IFD {}",
            fill - 50
        );
    }
}

#[test]
fn plane_shape_follows_dimension_order() {
    let shape = PlaneShape {
        z: 3,
        c: 2,
        t: 4,
        order: DimensionOrder::parse("XYCTZ").unwrap(),
    };
    // C fastest, then T, then Z.
    assert_eq!(shape.index(0, 1, 0), Some(1));
    assert_eq!(shape.index(0, 0, 1), Some(2));
    assert_eq!(shape.index(1, 0, 0), Some(8));
    assert_eq!(shape.index(3, 0, 0), None);
    for index in 0..shape.plane_count() {
        let (z, c, t) = shape.coords(index);
        assert_eq!(shape.index(z, c, t), Some(index));
    }
    assert!(DimensionOrder::parse("XYZZT").is_none());
    assert!(DimensionOrder::parse("ZCTXY").is_none());
}

#[test]
fn named_label_and_macro_images_become_associated_images() {
    let xml = ome(
        r#"<Image ID="Image:0" Name="slide"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="64" SizeY="48" SizeC="3" SizeZ="1" SizeT="1"><Channel SamplesPerPixel="3"/><TiffData IFD="0" PlaneCount="1"/></Pixels></Image><Image ID="Image:1" Name="label image"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="16" SizeY="16" SizeC="3" SizeZ="1" SizeT="1"><Channel SamplesPerPixel="3"/><TiffData IFD="1" PlaneCount="1"/></Pixels></Image><Image ID="Image:2" Name="macro"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="32" SizeY="16" SizeC="3" SizeZ="1" SizeT="1"><Channel SamplesPerPixel="3"/><TiffData IFD="2" PlaneCount="1"/></Pixels></Image>"#,
    );
    let file = build_tiff_tree(&[
        SyntheticIfd {
            tags: with_description(plane(64, 48, 16, 3, 8, 1), &xml),
            sub_ifds: vec![plane(32, 24, 16, 3, 8, 2)],
        },
        SyntheticIfd {
            tags: plane(16, 16, 16, 3, 8, 3),
            sub_ifds: Vec::new(),
        },
        SyntheticIfd {
            tags: plane(32, 16, 16, 3, 8, 4),
            sub_ifds: Vec::new(),
        },
    ]);
    let layout = interpret_file(&file).unwrap();

    assert_eq!(layout.dataset.scenes.len(), 1);
    assert_eq!(layout.dataset.scenes[0].name.as_deref(), Some("slide"));
    let label = &layout.dataset.associated_images["label"];
    assert_eq!((label.dimensions, label.channels), ((16, 16), 3));
    assert_eq!(
        layout.dataset.associated_images["macro"].dimensions,
        (32, 16)
    );
    assert_eq!(layout.associated_sources.len(), 2);

    let reader = TiffFamilyBackend::new().open(file.path()).unwrap();
    let label = reader.read_associated("label").unwrap();
    assert_eq!((label.width, label.height), (16, 16));
}

#[test]
fn other_images_become_named_scenes() {
    let image = |index: u32, name: &str| {
        format!(
            r#"<Image ID="Image:{index}" Name="{name}"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="32" SizeY="32" SizeC="3" SizeZ="1" SizeT="1"><Channel SamplesPerPixel="3"/><TiffData IFD="{index}" PlaneCount="1"/></Pixels></Image>"#
        )
    };
    let xml = ome(&format!("{}{}", image(0, "region A"), image(1, "region B")));
    let file = build_tiff_tree(&[
        SyntheticIfd {
            tags: with_description(plane(32, 32, 16, 3, 8, 5), &xml),
            sub_ifds: Vec::new(),
        },
        SyntheticIfd {
            tags: plane(32, 32, 16, 3, 8, 6),
            sub_ifds: Vec::new(),
        },
    ]);
    let layout = interpret_file(&file).unwrap();

    let names = layout
        .dataset
        .scenes
        .iter()
        .map(|scene| scene.name.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(names, [Some("region A"), Some("region B")]);
    assert!(layout.tile_sources.keys().any(|key| key.scene == 1));
}

fn single_plane_file(xml: &str) -> tempfile::NamedTempFile {
    build_tiff_tree(&[SyntheticIfd {
        tags: with_description(plane(64, 48, 16, 3, 8, 0), xml),
        sub_ifds: Vec::new(),
    }])
}

#[test]
fn unsupported_variants_fail_with_specific_errors() {
    let pixels = |attributes: &str, inner: &str| {
        ome(&format!(
            r#"<Image ID="Image:0"><Pixels DimensionOrder="XYCZT" SizeX="64" SizeY="48" SizeZ="1" SizeT="1" {attributes}>{inner}</Pixels></Image>"#
        ))
    };
    let rgb = r#"<Channel SamplesPerPixel="3"/>"#;
    let cases = [
        (
            pixels(
                r#"Type="uint8" SizeC="3""#,
                &format!(
                    r#"{rgb}<TiffData IFD="0" PlaneCount="1"><UUID FileName="other.ome.tif">urn:uuid:other</UUID></TiffData>"#
                ),
            ),
            "multi-file OME-TIFF",
        ),
        (
            ome(
                r#"<BinaryOnly MetadataFile="set.companion.ome" UUID="urn:uuid:x"/><Image ID="Image:0"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="64" SizeY="48" SizeC="3" SizeZ="1" SizeT="1"/></Image>"#,
            ),
            "BinaryOnly",
        ),
        (
            pixels(r#"Type="int16" SizeC="3""#, rgb),
            "pixel type 'int16'",
        ),
        (
            pixels(r#"Type="uint16" SizeC="3""#, rgb),
            "but OME-XML declares 3 uint16 samples",
        ),
        (
            pixels(r#"Type="uint8" SizeC="3" SizeX="65""#, rgb).replacen(r#"SizeX="64" "#, "", 1),
            "is 65x48 in OME-XML but 64x48",
        ),
        (
            pixels(
                r#"Type="uint8" SizeC="4""#,
                r#"<Channel SamplesPerPixel="4"/>"#,
            ),
            "4 samples per pixel",
        ),
        (
            pixels(
                r#"Type="uint8" SizeC="3""#,
                &format!(r#"{rgb}<TiffData IFD="5"/>"#),
            ),
            "references IFD 5",
        ),
        (
            pixels(
                r#"Type="uint8" SizeC="3""#,
                &format!(r#"{rgb}<TiffData IFD="0"/>"#),
            )
            .replace(
                "</OME>",
                r#"<Image ID="Image:1"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="64" SizeY="48" SizeC="3" SizeZ="1" SizeT="1"><Channel SamplesPerPixel="3"/><TiffData IFD="0"/></Pixels></Image></OME>"#,
            ),
            "Image 1 maps a plane to IFD 0, which already holds another plane",
        ),
    ];
    for (xml, expected) in cases {
        let error = interpret_error(&single_plane_file(&xml));
        assert!(
            error.contains(expected),
            "expected '{expected}' in: {error}"
        );
    }
}

#[test]
fn own_uuid_tiff_data_is_not_multi_file() {
    let xml = brightfield_xml("").replace(
        r#"<TiffData IFD="0" PlaneCount="1"/>"#,
        r#"<TiffData IFD="0" PlaneCount="1"><UUID FileName="renamed.ome.tif">urn:uuid:self</UUID></TiffData>"#,
    );
    let file = build_tiff_tree(&[SyntheticIfd {
        tags: with_description(plane(64, 48, 16, 3, 8, 0), &xml),
        sub_ifds: Vec::new(),
    }]);
    assert!(interpret_file(&file).is_ok());
}

#[test]
#[ignore = "requires WSI_RS_OME_TIFF_FIXTURE; see cargo xtask conformance-test"]
fn opens_ome_tiff_sub_ifd_pyramid_when_corpus_is_available() {
    let path = crate::test_support::required_fixture_path("WSI_RS_OME_TIFF_FIXTURE");

    let slide = crate::core::registry::Slide::open(&path).expect("open OME-TIFF");
    let dataset = slide.dataset();
    assert_eq!(dataset.properties.vendor(), Some("ome-tiff"));
    let series = &dataset.scenes[0].series[0];
    assert!(series.levels.len() > 1, "pyramid levels come from SubIFDs");
    assert!(series
        .levels
        .windows(2)
        .all(|pair| pair[1].downsample > pair[0].downsample));
    let lowest = series.levels.len() as u32 - 1;
    for c in 0..series.axes.c {
        for level in [0, lowest] {
            let tile = slide
                .read_tile(&tile_request(level, PlaneSelection::new(0, c, 0)))
                .expect("read OME-TIFF tile");
            assert!(tile.width() > 0 && tile.height() > 0);
        }
    }
}

#[test]
fn namespace_prefixed_ome_xml_parses() {
    let document = super::metadata::parse_ome_xml(&format!(
        r#"<ome:OME xmlns:ome="{OME_NS}"><ome:Image ID="Image:0"><ome:Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="4" SizeY="4" SizeC="1" SizeZ="1" SizeT="1"><ome:Channel SamplesPerPixel="1"/><ome:TiffData IFD="0"/></ome:Pixels></ome:Image></ome:OME>"#
    ))
    .unwrap();
    let pixels = &document.images[0].pixels;
    assert_eq!(pixels.channels.len(), 1);
    assert_eq!(pixels.tiff_data[0].ifd, Some(0));
}

#[test]
fn images_without_tiff_data_take_the_following_ifds() {
    let image = |index: u32| {
        format!(
            r#"<Image ID="Image:{index}"><Pixels DimensionOrder="XYCZT" Type="uint8" SizeX="32" SizeY="32" SizeC="3" SizeZ="1" SizeT="1"><Channel SamplesPerPixel="3"/></Pixels></Image>"#
        )
    };
    let xml = ome(&format!("{}{}", image(0), image(1)));
    let file = build_tiff_tree(&[
        SyntheticIfd {
            tags: with_description(plane(32, 32, 16, 3, 8, 7), &xml),
            sub_ifds: Vec::new(),
        },
        SyntheticIfd {
            tags: plane(32, 32, 16, 3, 8, 8),
            sub_ifds: Vec::new(),
        },
    ]);

    let reader = TiffFamilyBackend::new().open(file.path()).unwrap();
    for (scene, fill) in [(0usize, 7u8), (1, 8)] {
        let request = TileRequest {
            scene: scene.into(),
            ..tile_request(0, PlaneSelection::default())
        };
        let tile = reader.read_tile_cpu(&request).unwrap();
        assert!(
            tile.data
                .as_u8()
                .unwrap()
                .iter()
                .all(|&sample| sample == fill),
            "scene {scene} must decode IFD {scene}"
        );
    }
}
