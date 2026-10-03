//! OpenSlide's intermediate surface for a fractional tilemap subtile.

use crate::core::types::{cairo_bilinear_destination, ColorSpace, CpuTile, CpuTileLayout};
use crate::error::WsiError;

/// Pixman's 8-bit bilinear filter keeps 7 fractional weight bits.
const BILINEAR_INTERPOLATION_BITS: u32 = 7;

/// Copies the `size` window at fractional `origin` out of an opaque RGB8
/// source the way OpenSlide crops a Ventana subtile: Cairo lacks source
/// clipping, so it bilinearly paints the translated source OVER a clear
/// `ceil(width) x ceil(height)` ARGB32 surface. Pixman runs that composite on
/// its 8-bit path, so samples use 7-bit weights and truncate, and samples
/// past the source edge lose coverage. A fully covered surface is returned as
/// RGB; otherwise the result is straight-alpha RGBA, which region composition
/// treats as coverage.
pub(crate) fn cairo_subtile_surface_u8(
    source: &CpuTile,
    origin: (f64, f64),
    size: (u32, u32),
) -> Result<CpuTile, WsiError> {
    let Some(source_data) = source.data.as_u8().filter(|_| {
        source.channels == 3
            && source.color_space == ColorSpace::Rgb
            && source.layout == CpuTileLayout::Interleaved
    }) else {
        return Err(WsiError::DisplayConversion(
            "subtile surfaces require interleaved RGB8 sources".into(),
        ));
    };
    if !(origin.0.is_finite() && origin.1.is_finite()) || size.0 == 0 || size.1 == 0 {
        return Err(WsiError::DisplayConversion(format!(
            "invalid subtile window {}x{} at ({}, {})",
            size.0, size.1, origin.0, origin.1
        )));
    }
    // Cairo places the source at -origin, splitting the translation between
    // Pixman's integer offset and a 16.16 transform.
    let dest = (
        cairo_bilinear_destination(-origin.0),
        cairo_bilinear_destination(-origin.1),
    );
    let columns: Vec<_> = (0..size.0).map(|x| BilinearAxis::new(x, dest.0)).collect();
    let (source_width, source_height) = (i64::from(source.width), i64::from(source.height));
    let pixels = size.0 as usize * size.1 as usize;
    let mut premultiplied = Vec::with_capacity(pixels * 4);
    for y in 0..size.1 {
        let row = BilinearAxis::new(y, dest.1);
        for column in &columns {
            let taps = [
                (column.low, row.low),
                (column.low + 1, row.low),
                (column.low, row.low + 1),
                (column.low + 1, row.low + 1),
            ]
            .map(|(x, y)| {
                ((0..source_width).contains(&x) && (0..source_height).contains(&y)).then(|| {
                    let offset = (y as usize * source.width as usize + x as usize) * 3;
                    &source_data[offset..offset + 3]
                })
            });
            let weights = pixman_bilinear_weights(column.weight, row.weight);
            let interpolate = |sample: &dyn Fn(&[u8]) -> u8| {
                let sum: u32 = taps
                    .iter()
                    .zip(weights)
                    .map(|(tap, weight)| tap.map_or(0, |pixel| u32::from(sample(pixel)) * weight))
                    .sum();
                (sum >> 16) as u8
            };
            for channel in 0..3 {
                premultiplied.push(interpolate(&|pixel| pixel[channel]));
            }
            premultiplied.push(interpolate(&|_| u8::MAX));
        }
    }

    if premultiplied
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == u8::MAX)
    {
        let rgb = premultiplied
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        return CpuTile::from_u8_interleaved(size.0, size.1, 3, ColorSpace::Rgb, rgb);
    }
    for pixel in premultiplied.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u16::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .map_or(0, |straight| straight.min(255) as u8);
        }
    }
    CpuTile::from_u8_interleaved(size.0, size.1, 4, ColorSpace::Rgba, premultiplied)
}

/// One axis of a Pixman bilinear sample: the low source tap and the 7-bit
/// weight of the high tap, taken from the 16.16 sample position.
struct BilinearAxis {
    low: i64,
    weight: u32,
}

impl BilinearAxis {
    fn new(out: u32, dest: f64) -> Self {
        // Pixel center minus Pixman's half-pixel bilinear bias.
        let position = ((f64::from(out) - dest) * 65_536.0).round() as i64;
        Self {
            low: position >> 16,
            weight: ((position & 0xffff) >> (16 - BILINEAR_INTERPOLATION_BITS)) as u32,
        }
    }
}

/// Pixman `bilinear_interpolation` weights for the top-left, top-right,
/// bottom-left and bottom-right taps; they sum to 65536.
pub(super) fn pixman_bilinear_weights(x_weight: u32, y_weight: u32) -> [u32; 4] {
    let x = x_weight << (8 - BILINEAR_INTERPOLATION_BITS);
    let y = y_weight << (8 - BILINEAR_INTERPOLATION_BITS);
    [(256 - x) * (256 - y), x * (256 - y), (256 - x) * y, x * y]
}

#[cfg(test)]
#[path = "subtile/tests.rs"]
mod tests;
