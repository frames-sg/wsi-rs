use super::*;
use std::io::Write as _;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn raw_jp2k_source() -> tempfile::NamedTempFile {
    let mut source = tempfile::Builder::new().suffix(".j2c").tempfile().unwrap();
    source
        .write_all(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k"))
        .unwrap();
    source.flush().unwrap();
    source
}

#[test]
fn cache_grid_uses_each_supported_layout_geometry() {
    let regular = wsi_rs::Level::new(
        (1024, 768),
        1.0,
        TileLayout::Regular {
            tile_width: 128,
            tile_height: 256,
            tiles_across: 8,
            tiles_down: 3,
        },
    );
    assert_eq!(cache_grid(&regular).unwrap(), (128, 256, 8, 3));

    let whole = wsi_rs::Level::new(
        (513, 257),
        1.0,
        TileLayout::WholeLevel {
            width: 513,
            height: 257,
            virtual_tile_width: 512,
            virtual_tile_height: 512,
        },
    );
    assert_eq!(cache_grid(&whole).unwrap(), (256, 256, 3, 2));

    let irregular = wsi_rs::Level::new(
        (513, 257),
        1.0,
        TileLayout::Irregular {
            tile_advance: (128.0, 128.0),
            extra_tiles: (0, 0, 0, 0),
            tiles: std::collections::HashMap::new(),
        },
    );
    assert_eq!(cache_grid(&irregular).unwrap(), (256, 256, 3, 2));
}

#[test]
fn numeric_size_and_pair_parsers_reject_malformed_values() {
    assert_eq!(parse_u32_option("level", "42").unwrap(), 42);
    assert!(parse_u32_option("level", "-1")
        .unwrap_err()
        .contains("level"));
    assert_eq!(parse_u64_option("margin", "42").unwrap(), 42);
    assert!(parse_u64_option("margin", "nope")
        .unwrap_err()
        .contains("margin"));

    assert_eq!(parse_size("640x480").unwrap(), (640, 480));
    for value in ["640", "0x480", "640x0", "badx480"] {
        assert!(parse_size(value).is_err(), "{value} should be rejected");
    }
    assert_eq!(parse_pair("12,34", "--origin").unwrap(), (12, 34));
    for value in ["12", "bad,34", "12,bad"] {
        assert!(
            parse_pair(value, "--origin").is_err(),
            "{value} should be rejected"
        );
    }
}

#[test]
fn build_window_parser_accepts_every_option_and_alias() {
    let parsed = parse_build_window_args(&args(&[
        "slide.svs",
        "--out",
        "cache.svcache",
        "--level",
        "2",
        "--z",
        "3",
        "--viewport",
        "640x480",
        "--origin",
        "12,34",
        "--margin-tiles",
        "4",
    ]))
    .unwrap();

    assert_eq!(parsed.slide_path, PathBuf::from("slide.svs"));
    assert_eq!(parsed.out_path, Some(PathBuf::from("cache.svcache")));
    assert_eq!(parsed.level, 2);
    assert_eq!(parsed.z, 3);
    assert_eq!(parsed.size, (640, 480));
    assert_eq!(parsed.origin, Some((12, 34)));
    assert_eq!(parsed.center, None);
    assert_eq!(parsed.margin_tiles, 4);
}

#[test]
fn build_window_parser_reports_each_missing_or_conflicting_input() {
    for (values, expected) in [
        (vec!["slide.svs"], "--size"),
        (vec!["--size", "1x1"], "usage:"),
        (vec!["slide.svs", "--out"], "--out requires"),
        (vec!["slide.svs", "--level"], "--level requires"),
        (vec!["slide.svs", "--z"], "--z requires"),
        (vec!["slide.svs", "--size"], "--size requires"),
        (vec!["slide.svs", "--origin"], "--origin requires"),
        (vec!["slide.svs", "--center"], "--center requires"),
        (
            vec!["slide.svs", "--margin-tiles"],
            "--margin-tiles requires",
        ),
        (vec!["slide.svs", "--unknown"], "unknown option"),
        (
            vec!["one.svs", "two.svs", "--size", "1x1"],
            "unexpected argument",
        ),
        (
            vec![
                "slide.svs",
                "--size",
                "1x1",
                "--origin",
                "0,0",
                "--center",
                "0,0",
            ],
            "mutually exclusive",
        ),
    ] {
        let error = parse_build_window_args(&args(&values)).unwrap_err();
        assert!(
            error.contains(expected),
            "expected {expected:?} for {values:?}, got {error:?}"
        );
    }
}

#[test]
fn build_parser_and_window_builder_surface_contextual_errors() {
    assert!(build(&[]).unwrap_err().contains("usage:"));
    assert!(build(&args(&["slide.svs", "--out"]))
        .unwrap_err()
        .contains("--out requires"));
    assert!(build(&args(&["slide.svs", "--unknown"]))
        .unwrap_err()
        .contains("unknown option"));
    assert!(build(&args(&["one.svs", "two.svs"]))
        .unwrap_err()
        .contains("unexpected argument"));
    assert!(build(&args(&["definitely-missing.svs"]))
        .unwrap_err()
        .contains("I/O"));

    let error = build_window(&args(&["definitely-missing.svs", "--size", "1x1"])).unwrap_err();
    assert!(error.contains("open definitely-missing.svs"));
}

#[test]
fn window_origin_honors_explicit_center_and_default_modes() {
    assert_eq!(
        window_origin((1000, 800), (200, 100), Some((9, 8)), None),
        (9, 8)
    );
    assert_eq!(
        window_origin((1000, 800), (200, 100), None, None),
        (400, 350)
    );
    assert_eq!(
        window_origin((1000, 800), (200, 100), None, Some((100, 50))),
        (0, 0)
    );
    assert_eq!(window_origin((10, 10), (20, 20), None, None), (0, 0));
}

#[test]
fn command_dispatch_covers_help_unknown_and_argument_errors() {
    assert!(dispatch(Vec::new()).unwrap_err().contains("usage:"));
    assert!(dispatch(args(&["unknown"]))
        .unwrap_err()
        .contains("unknown command"));
    assert!(dispatch(args(&["build"])).unwrap_err().contains("usage:"));
    assert!(dispatch(args(&["build-window"]))
        .unwrap_err()
        .contains("--size"));
    dispatch(args(&["--help"])).unwrap();
    dispatch(args(&["-h"])).unwrap();
}

#[test]
fn complete_and_window_build_commands_write_readable_caches() {
    let source = raw_jp2k_source();
    let dir = tempfile::tempdir().unwrap();
    let complete = dir.path().join("complete.svcache");
    build(&args(&[
        source.path().to_str().unwrap(),
        "--out",
        complete.to_str().unwrap(),
    ]))
    .unwrap();
    assert!(complete.is_file());

    let window = dir.path().join("window.svcache");
    build_window(&args(&[
        source.path().to_str().unwrap(),
        "--size",
        "1x1",
        "--center",
        "0,0",
        "--margin-tiles",
        "0",
        "--out",
        window.to_str().unwrap(),
    ]))
    .unwrap();
    assert!(window.is_file());

    let slide = Slide::open(&window).unwrap();
    assert_eq!(slide.dataset().scenes.len(), 1);
    assert_eq!(slide.dataset().scenes[0].series[0].levels.len(), 1);
}
