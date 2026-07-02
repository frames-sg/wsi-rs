pub(crate) fn irregular_extra_tiles(
    offset_x: f64,
    offset_y: f64,
    tile_advance_x: f64,
    tile_advance_y: f64,
    tile_width: f64,
    tile_height: f64,
) -> (u32, u32, u32, u32) {
    let extra_right = if offset_x < 0.0 {
        (-offset_x / tile_advance_x).ceil() as u32
    } else {
        0
    };
    let offset_xr = offset_x + (tile_width - tile_advance_x);
    let extra_left = if offset_xr > 0.0 {
        (offset_xr / tile_advance_x).ceil() as u32
    } else {
        0
    };
    let extra_bottom = if offset_y < 0.0 {
        (-offset_y / tile_advance_y).ceil() as u32
    } else {
        0
    };
    let offset_yr = offset_y + (tile_height - tile_advance_y);
    let extra_top = if offset_yr > 0.0 {
        (offset_yr / tile_advance_y).ceil() as u32
    } else {
        0
    };
    (extra_top, extra_bottom, extra_left, extra_right)
}

#[cfg(test)]
mod tests {
    use super::irregular_extra_tiles;

    #[test]
    fn irregular_extra_tiles_reports_each_side_independently() {
        assert_eq!(
            irregular_extra_tiles(-12.0, -7.0, 10.0, 5.0, 14.0, 9.0),
            (0, 2, 0, 2)
        );
        assert_eq!(
            irregular_extra_tiles(3.0, 2.0, 10.0, 5.0, 14.0, 9.0),
            (2, 0, 1, 0)
        );
        assert_eq!(
            irregular_extra_tiles(0.0, 0.0, 10.0, 5.0, 10.0, 5.0),
            (0, 0, 0, 0)
        );
    }
}
