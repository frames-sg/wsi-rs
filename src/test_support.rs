use crate::{
    AxesShape, ChannelInfo, CpuTile, Dataset, DatasetId, Level, LevelIdx, PlaneIdx, PlaneSelection,
    RegionRequest, SampleType, Scene, SceneId, Series, SeriesId, TileLayout,
};

pub(crate) struct RegularLevelForTest {
    pub(crate) dimensions: (u64, u64),
    pub(crate) tile_width: u32,
    pub(crate) tile_height: u32,
    pub(crate) tiles_across: u64,
    pub(crate) tiles_down: u64,
}

pub(crate) fn rgb_channels_for_test() -> Vec<ChannelInfo> {
    ["R", "G", "B"]
        .into_iter()
        .map(|name| ChannelInfo {
            name: Some(name.into()),
            color: None,
            excitation_nm: None,
            emission_nm: None,
        })
        .collect()
}

pub(crate) fn regular_rgb_dataset_for_test(
    dataset_id: DatasetId,
    scene_id: &str,
    series_id: &str,
    level: RegularLevelForTest,
) -> Dataset {
    Dataset::new(
        dataset_id,
        vec![Scene {
            id: scene_id.into(),
            name: None,
            series: vec![Series::new(
                series_id,
                AxesShape::default(),
                vec![Level::new(
                    level.dimensions,
                    1.0,
                    TileLayout::Regular {
                        tile_width: level.tile_width,
                        tile_height: level.tile_height,
                        tiles_across: level.tiles_across,
                        tiles_down: level.tiles_down,
                    },
                )],
                SampleType::Uint8,
                rgb_channels_for_test(),
            )],
        }],
    )
}

pub(crate) fn assert_cpu_tile_matches_rgb_fixture_with_tolerance(
    image: &CpuTile,
    expected_rgb: &image::RgbImage,
    max_channel_delta: u8,
    max_avg_channel_delta_x100: u64,
    label: &str,
) {
    assert_eq!(image.width, expected_rgb.width());
    assert_eq!(image.height, expected_rgb.height());
    let actual = image.data.as_u8().unwrap();
    let expected = expected_rgb.as_raw();
    assert_eq!(actual.len(), expected.len());

    let mut total_delta = 0u64;
    let mut max_delta = 0u8;
    for (actual, expected) in actual.iter().zip(expected.iter()) {
        let delta = actual.abs_diff(*expected);
        total_delta += u64::from(delta);
        max_delta = max_delta.max(delta);
    }

    let avg_delta_x100 = if actual.is_empty() {
        0
    } else {
        (total_delta * 100) / actual.len() as u64
    };

    assert!(
        max_delta <= max_channel_delta,
        "{label} drift too large: max channel delta {max_delta} > {max_channel_delta}",
    );
    assert!(
        avg_delta_x100 <= max_avg_channel_delta_x100,
        "{label} drift too large: average channel delta {:.2} > {:.2}",
        avg_delta_x100 as f64 / 100.0,
        max_avg_channel_delta_x100 as f64 / 100.0,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn region_request(
    scene: usize,
    series: usize,
    level: u32,
    plane: PlaneSelection,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> RegionRequest {
    RegionRequest {
        scene: SceneId::new(scene),
        series: SeriesId::new(series),
        level: LevelIdx::new(level),
        plane: PlaneIdx::new(plane),
        origin_px: (x, y),
        size_px: (w, h),
    }
}
