use super::*;
use crate::core::registry::Slide;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};
use std::path::Path;

fn minimal_dataset(scene_count: usize) -> Dataset {
    Dataset {
        id: DatasetId(1),
        scenes: (0..scene_count)
            .map(|scene_idx| Scene {
                id: format!("s{scene_idx}"),
                name: None,
                series: vec![Series {
                    id: "ser0".into(),
                    axes: AxesShape::default(),
                    levels: Vec::new(),
                    sample_type: SampleType::Uint8,
                    channels: Vec::new(),
                }],
            })
            .collect(),
        associated_images: HashMap::new(),
        properties: Properties::new(),
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

fn leica_image_for_ifd(ifd_index: usize) -> LeicaImageInfo {
    LeicaImageInfo {
        name: None,
        ifd_levels: vec![ParsedDimension {
            ifd_index,
            width: 64,
            height: 64,
            r: 0,
            z: 0,
            c: 0,
        }],
        channels: Vec::new(),
        view_size: (64, 64),
        view_offset: (0, 0),
        creation_date: None,
        device_model: None,
        device_version: None,
        objective: None,
        numerical_aperture: None,
        illumination_source: Some("brightfield".into()),
        is_macro: false,
    }
}

fn tiled_ifd(width: u32, height: u32, sample_bits: u16, icc: Option<Vec<u8>>) -> Vec<SyntheticTag> {
    let mut tiff_tags = vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::long(tags::TILE_WIDTH, 128),
        SyntheticTag::long(tags::TILE_LENGTH, 64),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::short(tags::BITS_PER_SAMPLE, sample_bits),
    ];
    if let Some(icc) = icc {
        tiff_tags.push(SyntheticTag::bytes(tags::ICC_PROFILE, icc));
    }
    tiff_tags
}

fn full_leica_xml() -> &'static str {
    r##"<scn xmlns="http://www.leica-microsystems.com/scn/2010/10/01">
        <collection sizeX="600000" sizeY="400000">
            <barcode>U0xJREUxMjM=</barcode>
            <image name="Fluorescence scene">
                <dimension ifd="0" sizeX="512" sizeY="256" r="0" z="0" c="0"/>
                <dimension ifd="1" sizeX="256" sizeY="128" r="1" z="0" c="0"/>
                <dimension ifd="2" sizeX="512" sizeY="256" r="0" z="0" c="1"/>
                <view sizeX="512000" sizeY="256000" offsetX="1000" offsetY="2000"/>
                <creationDate>2026-01-02T03:04:05Z</creationDate>
                <device model="GT450" version="1.2.3"/>
                <scanSettings>
                    <objectiveSettings><objective>40</objective></objectiveSettings>
                    <illuminationSettings>
                        <numericalAperture>0.95</numericalAperture>
                        <illuminationSource>fluorescence</illuminationSource>
                    </illuminationSettings>
                    <channelSettings>
                        <channel index="0" name="DAPI" rgb="#0000ff"/>
                        <channel index="1" name="FITC" rgb="00ff00"/>
                    </channelSettings>
                </scanSettings>
            </image>
            <image name="Overview">
                <dimension ifd="3" sizeX="120" sizeY="80" r="0" z="0" c="0"/>
                <view type="macro" sizeX="600000" sizeY="400000" offsetX="0" offsetY="0"/>
            </image>
        </collection>
    </scn>"##
}

#[test]
fn interpret_builds_multichannel_scene_macro_and_public_metadata() {
    let scene_icc = vec![1, 3, 5, 7, 9];
    let mut first = tiled_ifd(512, 256, 16, Some(scene_icc.clone()));
    first.push(SyntheticTag::ascii(
        tags::IMAGE_DESCRIPTION,
        full_leica_xml(),
    ));
    let file = build_tiff(&[
        first,
        tiled_ifd(256, 128, 16, None),
        tiled_ifd(512, 256, 16, None),
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 120),
            SyntheticTag::long(tags::IMAGE_LENGTH, 80),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 0),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 0),
            SyntheticTag::short(tags::BITS_PER_SAMPLE, 16),
            SyntheticTag::short(tags::SAMPLES_PER_PIXEL, 4),
        ],
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = LeicaInterpreter;

    assert!(interpreter.detect(&container));
    assert_eq!(interpreter.vendor_name(), "leica");
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(layout.dataset.scenes.len(), 1);
    let scene = &layout.dataset.scenes[0];
    assert_eq!(scene.name.as_deref(), Some("Fluorescence scene"));
    let series = &scene.series[0];
    assert_eq!(series.axes, AxesShape { z: 1, c: 2, t: 1 });
    assert_eq!(series.sample_type, SampleType::Uint16);
    assert_eq!(series.levels.len(), 2);
    assert_eq!(series.levels[0].dimensions, (512, 256));
    assert_eq!(series.levels[1].dimensions, (256, 128));
    assert_eq!(series.channels[0].name.as_deref(), Some("DAPI"));
    assert_eq!(series.channels[0].color, Some([0, 0, 255]));
    assert_eq!(series.channels[1].name.as_deref(), Some("FITC"));
    assert_eq!(series.channels[1].color, Some([0, 255, 0]));
    assert_eq!(layout.tile_sources.len(), 3);

    let macro_image = &layout.dataset.associated_images["macro"];
    assert_eq!(macro_image.dimensions, (120, 80));
    assert_eq!(macro_image.sample_type, SampleType::Uint16);
    assert_eq!(macro_image.channels, 4);
    assert!(matches!(
        layout.associated_sources["macro"],
        TileSource::Stripped { .. }
    ));

    let properties = &layout.dataset.properties;
    assert_eq!(properties.get("leica.barcode"), Some("SLIDE123"));
    assert_eq!(properties.get("openslide.barcode"), Some("SLIDE123"));
    assert_eq!(
        properties.get("leica.creation-date"),
        Some("2026-01-02T03:04:05Z")
    );
    assert_eq!(properties.get("leica.device-model"), Some("GT450"));
    assert_eq!(properties.get("leica.device-version"), Some("1.2.3"));
    assert_eq!(properties.get("leica.objective"), Some("40"));
    assert_eq!(properties.get("leica.aperture"), Some("0.95"));
    assert_eq!(
        properties.get("leica.illumination-source"),
        Some("fluorescence")
    );
    assert_eq!(properties.get("openslide.mpp-x"), Some("1"));
    assert_eq!(properties.get("openslide.mpp-y"), Some("1"));
    assert_eq!(properties.get("openslide.region[0].x"), Some("1"));
    assert_eq!(properties.get("openslide.region[0].y"), Some("2"));
    assert_eq!(properties.get("leica.collection-size-x"), Some("600000"));
    assert_eq!(properties.get("leica.collection-size-y"), Some("400000"));
    assert_eq!(properties.get("leica.scene[0].view-size-x"), Some("512000"));
    assert_eq!(properties.get("leica.scene[0].view-size-y"), Some("256000"));
    assert_eq!(properties.get("leica.scene[0].offset-x"), Some("1000"));
    assert_eq!(properties.get("leica.scene[0].offset-y"), Some("2000"));
    assert_eq!(layout.dataset.source_icc_profiles[0].bytes, scene_icc);
}

#[test]
fn interpret_rejects_invalid_xml_structure_and_ifd_references() {
    for (description, expected) in [
        ("<scn/>", "no <collection>"),
        ("<collection sizeX=\"1\" sizeY=\"1\"/>", "no <image>"),
        (
            "<collection sizeX=\"bad\" sizeY=\"1\"><image/></collection>",
            "invalid integer 'bad'",
        ),
    ] {
        let file = build_tiff(&[vec![SyntheticTag::ascii(
            tags::IMAGE_DESCRIPTION,
            description,
        )]]);
        let container = TiffContainer::open(file.path()).unwrap();
        assert!(LeicaInterpreter
            .interpret(&container)
            .unwrap_err()
            .to_string()
            .contains(expected));
    }

    let xml = r#"<collection sizeX="100" sizeY="100"><image>
        <dimension ifd="9" sizeX="10" sizeY="10" r="0"/>
        <view sizeX="50" sizeY="50" offsetX="1" offsetY="1"/>
    </image></collection>"#;
    let file = build_tiff(&[vec![
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, xml),
        SyntheticTag::long(tags::TILE_WIDTH, 8),
        SyntheticTag::long(tags::TILE_LENGTH, 8),
    ]]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(LeicaInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("out-of-range IFD index 9"));
}

#[test]
fn parse_image_info_collects_dimensions_once_and_propagates_errors() {
    let image = xml::parse_xml(
        r#"<image><dimension ifd="bad" sizeX="10" sizeY="10"/><view sizeX="10" sizeY="10"/></image>"#,
    )
    .unwrap();
    let error = match LeicaInterpreter.parse_image_info(&image, (20, 20)) {
        Err(error) => error,
        Ok(_) => panic!("invalid IFD index unexpectedly parsed"),
    };
    assert!(error.to_string().contains("invalid ifd index 'bad'"));

    let image = xml::parse_xml(
        r#"<image><dimension ifd="0" sizeX="10" sizeY="10"/><view sizeX="10" sizeY="10"/></image>"#,
    )
    .unwrap();
    let parsed = LeicaInterpreter
        .parse_image_info(&image, (20, 20))
        .unwrap()
        .unwrap();
    assert_eq!(parsed.ifd_levels.len(), 1);
}

#[test]
fn detect_scn_with_namespace() {
    let xml = format!(
        r#"<scn xmlns="{}"><collection sizeX="1000" sizeY="1000"></collection></scn>"#,
        LEICA_NS_2010_10
    );
    let root = xml::parse_xml(&xml).unwrap();
    assert_eq!(root.find("collection").unwrap().attr("sizeX"), Some("1000"));
}

#[test]
fn parse_dimension_extracts_resolution_index() {
    let interp = LeicaInterpreter;
    let node = xml::XmlNode {
        tag: "dimension".into(),
        attributes: HashMap::from([
            ("ifd".into(), "2".into()),
            ("sizeX".into(), "4096".into()),
            ("sizeY".into(), "3072".into()),
            ("r".into(), "3".into()),
            ("z".into(), "0".into()),
            ("c".into(), "2".into()),
        ]),
        text: None,
        children: vec![],
    };

    let parsed = interp.parse_dimension(&node).unwrap().unwrap();
    assert_eq!(parsed.ifd_index, 2);
    assert_eq!(parsed.width, 4096);
    assert_eq!(parsed.height, 3072);
    assert_eq!(parsed.r, 3);
    assert_eq!(parsed.z, 0);
    assert_eq!(parsed.c, 2);
}

#[test]
fn parse_channel_settings_extracts_names_and_colors() {
    let image = xml::XmlNode {
        tag: "image".into(),
        attributes: HashMap::new(),
        text: None,
        children: vec![xml::XmlNode {
            tag: "scanSettings".into(),
            attributes: HashMap::new(),
            text: None,
            children: vec![xml::XmlNode {
                tag: "channelSettings".into(),
                attributes: HashMap::new(),
                text: None,
                children: vec![xml::XmlNode {
                    tag: "channel".into(),
                    attributes: HashMap::from([
                        ("index".into(), "2".into()),
                        ("name".into(), "TX2|Empty".into()),
                        ("rgb".into(), "#ff0000".into()),
                    ]),
                    text: None,
                    children: vec![],
                }],
            }],
        }],
    };

    let channels = parse_channel_settings(&image);
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].index, 2);
    assert_eq!(channels[0].info.name.as_deref(), Some("TX2|Empty"));
    assert_eq!(channels[0].info.color, Some([255, 0, 0]));
}

#[test]
fn macro_detection_uses_collection_geometry() {
    let interp = LeicaInterpreter;
    let image = xml::XmlNode {
        tag: "image".into(),
        attributes: HashMap::new(),
        text: None,
        children: vec![
            xml::XmlNode {
                tag: "pixels".into(),
                attributes: HashMap::from([
                    ("sizeX".into(), "100".into()),
                    ("sizeY".into(), "200".into()),
                ]),
                text: None,
                children: vec![xml::XmlNode {
                    tag: "dimension".into(),
                    attributes: HashMap::from([
                        ("ifd".into(), "0".into()),
                        ("sizeX".into(), "100".into()),
                        ("sizeY".into(), "200".into()),
                        ("r".into(), "0".into()),
                    ]),
                    text: None,
                    children: vec![],
                }],
            },
            xml::XmlNode {
                tag: "view".into(),
                attributes: HashMap::from([
                    ("sizeX".into(), "1000".into()),
                    ("sizeY".into(), "2000".into()),
                    ("offsetX".into(), "0".into()),
                    ("offsetY".into(), "0".into()),
                ]),
                text: None,
                children: vec![],
            },
            xml::XmlNode {
                tag: "scanSettings".into(),
                attributes: HashMap::new(),
                text: None,
                children: vec![xml::XmlNode {
                    tag: "illuminationSettings".into(),
                    attributes: HashMap::new(),
                    text: None,
                    children: vec![xml::XmlNode {
                        tag: "illuminationSource".into(),
                        attributes: HashMap::new(),
                        text: Some("brightfield".into()),
                        children: vec![],
                    }],
                }],
            },
        ],
    };

    let parsed = interp
        .parse_image_info(&image, (1000, 2000))
        .unwrap()
        .unwrap();
    assert!(parsed.is_macro);
}

#[test]
fn public_properties_use_axis_specific_cpp() {
    let interp = LeicaInterpreter;
    let collection = xml::XmlNode {
        tag: "collection".into(),
        attributes: HashMap::new(),
        text: None,
        children: vec![],
    };
    let main_images = vec![LeicaImageInfo {
        name: Some("main".into()),
        ifd_levels: vec![ParsedDimension {
            ifd_index: 0,
            width: 100,
            height: 50,
            r: 0,
            z: 0,
            c: 0,
        }],
        channels: vec![],
        view_size: (1000, 600),
        view_offset: (200, 150),
        creation_date: None,
        device_model: None,
        device_version: None,
        objective: None,
        numerical_aperture: None,
        illumination_source: Some("brightfield".into()),
        is_macro: false,
    }];

    let props = interp
        .parse_public_properties(&collection, (2_000, 1_200), &main_images, 10.0, 12.0)
        .unwrap();
    assert_eq!(props.get("openslide.mpp-x"), Some("0.01"));
    assert_eq!(props.get("openslide.mpp-y"), Some("0.012"));
}

#[test]
fn public_geometry_uses_collection_nm_per_pixel_for_both_axes() {
    let _main_images = [LeicaImageInfo {
        name: Some("main".into()),
        ifd_levels: vec![
            ParsedDimension {
                ifd_index: 0,
                width: 36832,
                height: 38432,
                r: 0,
                z: 0,
                c: 0,
            },
            ParsedDimension {
                ifd_index: 1,
                width: 9208,
                height: 9608,
                r: 1,
                z: 0,
                c: 0,
            },
            ParsedDimension {
                ifd_index: 2,
                width: 2302,
                height: 2402,
                r: 2,
                z: 0,
                c: 0,
            },
            ParsedDimension {
                ifd_index: 3,
                width: 576,
                height: 600,
                r: 3,
                z: 0,
                c: 0,
            },
            ParsedDimension {
                ifd_index: 4,
                width: 144,
                height: 150,
                r: 4,
                z: 0,
                c: 0,
            },
        ],
        channels: vec![],
        view_size: (18416000, 19217000),
        view_offset: (5389341, 17548313),
        creation_date: None,
        device_model: None,
        device_version: None,
        objective: None,
        numerical_aperture: None,
        illumination_source: Some("brightfield".into()),
        is_macro: false,
    }];
    let level0_cpp_x = 18416000.0 / 36832.0;
    let level0_cpp_y = 19217000.0 / 38432.0;
    let level3_cpp: f64 = 18416000.0 / 576.0;
    let width = (26564529.0_f64 / level3_cpp).ceil() as u64;
    let height = (76734666.0_f64 / level3_cpp).ceil() as u64;
    assert_eq!(width, 831);
    assert_eq!(height, 2401);
    assert!(level0_cpp_y > level0_cpp_x);
}

#[test]
fn source_icc_profiles_are_attached_per_scene() {
    let scene0_icc = vec![1, 2, 3, 4, 5];
    let scene1_icc = vec![6, 7, 8, 9, 10];
    let file = build_tiff(&[
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 64),
            SyntheticTag::long(tags::IMAGE_LENGTH, 64),
            SyntheticTag::bytes(tags::ICC_PROFILE, scene0_icc.clone()),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 64),
            SyntheticTag::long(tags::IMAGE_LENGTH, 64),
            SyntheticTag::bytes(tags::ICC_PROFILE, scene1_icc.clone()),
        ],
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let mut dataset = minimal_dataset(2);
    let main_images = vec![leica_image_for_ifd(0), leica_image_for_ifd(1)];

    attach_source_icc_profiles_for_main_images(
        &mut dataset,
        &container,
        container.top_ifds(),
        &main_images,
    )
    .unwrap();

    assert_eq!(dataset.source_icc_profiles.len(), 2);
    assert_eq!(dataset.source_icc_profiles[0].key.scene, SceneId::new(0));
    assert_eq!(dataset.source_icc_profiles[0].bytes, scene0_icc);
    assert_eq!(dataset.source_icc_profiles[1].key.scene, SceneId::new(1));
    assert_eq!(dataset.source_icc_profiles[1].bytes, scene1_icc);
    assert_eq!(
        dataset
            .icc_profiles
            .get(&IccProfileKey::new(SceneId::new(0), SeriesId::new(0))),
        Some(&scene0_icc)
    );
    assert_eq!(
        dataset
            .icc_profiles
            .get(&IccProfileKey::new(SceneId::new(1), SeriesId::new(0))),
        Some(&scene1_icc)
    );
}

#[test]
fn opens_dissimilar_leica_scenes_when_corpus_is_available() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path = workspace_root.join("downloads/openslide-testdata/Leica/Leica-3.scn");
    if !path.exists() {
        return;
    }

    let slide = Slide::open(&path).expect("open Leica multi-scene SCN");
    let dataset = slide.dataset();
    assert!(dataset.scenes.len() > 1);
    assert!(dataset.associated_images.contains_key("macro"));
    let first_series = &dataset.scenes[0].series[0];
    assert_eq!(first_series.axes, AxesShape::default());
    assert!(!first_series.levels.is_empty());
}

#[test]
fn opens_fluorescence_leica_channels_when_corpus_is_available() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path = workspace_root.join("downloads/openslide-testdata/Leica/Leica-Fluorescence-1.scn");
    if !path.exists() {
        return;
    }

    let slide = Slide::open(&path).expect("open Leica fluorescence SCN");
    let dataset = slide.dataset();
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes.c, 3);
    assert_eq!(series.channels.len(), 3);
    assert_eq!(series.channels[0].color, Some([0, 0, 255]));
    for c in 0..series.axes.c {
        let tile = slide
            .read_tile(&TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection { z: 0, c, t: 0 }.into(),
                col: 0,
                row: 0,
            })
            .expect("read fluorescence channel tile");
        assert!(tile.width() > 0);
        assert!(tile.height() > 0);
    }
}
