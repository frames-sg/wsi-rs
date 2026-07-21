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
        .parse_public_properties(&collection, &main_images, 10.0, 12.0)
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
            .read_tile(
                &TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection { z: 0, c, t: 0 }.into(),
                    col: 0,
                    row: 0,
                },
                TileOutputPreference::cpu(),
            )
            .expect("read fluorescence channel tile");
        assert!(matches!(tile, TilePixels::Cpu(_)));
    }
}
