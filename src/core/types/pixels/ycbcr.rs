//! Full-range YCbCr to RGB conversion. Eight-bit samples go through the
//! lookup tables OpenSlide 4.0.1 uses, so CPU, Metal and OpenSlide agree to
//! the bit.

const CHROMA_VALUES: usize = 256;

// Full-range YCbCr to RGB coefficients as integer ratios: the red and blue
// terms are over `RED_BLUE_SCALE`, the green terms over `GREEN_SCALE`.
const RED_FROM_CR: i64 = 1_402;
const BLUE_FROM_CB: i64 = 1_772;
const RED_BLUE_SCALE: i64 = 1_000;
const GREEN_FROM_CB: i64 = 34_414;
const GREEN_FROM_CR: i64 = 71_414;
const GREEN_SCALE: i64 = 100_000;

struct YcbcrTables {
    red_from_cr: [i16; CHROMA_VALUES],
    green_from_cb: [i32; CHROMA_VALUES],
    green_from_cr: [i32; CHROMA_VALUES],
    blue_from_cb: [i16; CHROMA_VALUES],
}

const fn round_ratio(numerator: i64, denominator: i64) -> i64 {
    if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        -((-numerator + denominator / 2) / denominator)
    }
}

const fn build_ycbcr_tables() -> YcbcrTables {
    let mut tables = YcbcrTables {
        red_from_cr: [0; CHROMA_VALUES],
        green_from_cb: [0; CHROMA_VALUES],
        green_from_cr: [0; CHROMA_VALUES],
        blue_from_cb: [0; CHROMA_VALUES],
    };
    let mut index = 0;
    while index < CHROMA_VALUES {
        let chroma = index as i64 - 128;
        tables.red_from_cr[index] = round_ratio(RED_FROM_CR * chroma, RED_BLUE_SCALE) as i16;
        tables.green_from_cb[index] = round_ratio(
            65_536 * (GREEN_SCALE / 2 - GREEN_FROM_CB * chroma),
            GREEN_SCALE,
        ) as i32;
        tables.green_from_cr[index] =
            round_ratio(65_536 * (-GREEN_FROM_CR * chroma), GREEN_SCALE) as i32;
        tables.blue_from_cb[index] = round_ratio(BLUE_FROM_CB * chroma, RED_BLUE_SCALE) as i16;
        index += 1;
    }
    tables
}

// OpenSlide 4.0.1 uses these lookup-table values, including the precomputed
// half-unit in the green Cb term, before combining green in 16-bit fixed point.
static YCBCR_TABLES: YcbcrTables = build_ycbcr_tables();

/// Constant-buffer order shared with the Metal converter: red, green Cb,
/// green Cr, blue. Widening the short terms preserves every CPU table value.
#[cfg(feature = "metal")]
pub(crate) fn ycbcr_shader_tables() -> [i32; CHROMA_VALUES * 4] {
    let mut values = [0; CHROMA_VALUES * 4];
    for index in 0..CHROMA_VALUES {
        values[index] = i32::from(YCBCR_TABLES.red_from_cr[index]);
        values[CHROMA_VALUES + index] = YCBCR_TABLES.green_from_cb[index];
        values[CHROMA_VALUES * 2 + index] = YCBCR_TABLES.green_from_cr[index];
        values[CHROMA_VALUES * 3 + index] = i32::from(YCBCR_TABLES.blue_from_cb[index]);
    }
    values
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// One 8-bit full-range YCbCr pixel as RGB.
#[inline]
pub(crate) fn ycbcr_to_rgb_u8(yy: u8, cb: u8, cr: u8) -> [u8; 3] {
    let yy = i32::from(yy);
    let cb = usize::from(cb);
    let cr = usize::from(cr);
    [
        clamp_u8(yy + i32::from(YCBCR_TABLES.red_from_cr[cr])),
        clamp_u8(yy + ((YCBCR_TABLES.green_from_cb[cb] + YCBCR_TABLES.green_from_cr[cr]) >> 16)),
        clamp_u8(yy + i32::from(YCBCR_TABLES.blue_from_cb[cb])),
    ]
}

/// One full-range YCbCr pixel of `bits_stored`-bit samples (1 to 16) as RGB.
/// Each term is rounded from the exact ratio; no table covers 16 bits.
pub(crate) fn ycbcr_to_rgb_u16(y: u16, cb: u16, cr: u16, bits_stored: u8) -> [u16; 3] {
    let midpoint = 1_i64 << (bits_stored - 1);
    let max = (1_i64 << bits_stored) - 1;
    let y = i64::from(y);
    let cb = i64::from(cb) - midpoint;
    let cr = i64::from(cr) - midpoint;
    let red = y + round_ratio(RED_FROM_CR * cr, RED_BLUE_SCALE);
    let green = y - round_ratio(GREEN_FROM_CB * cb + GREEN_FROM_CR * cr, GREEN_SCALE);
    let blue = y + round_ratio(BLUE_FROM_CB * cb, RED_BLUE_SCALE);
    [
        red.clamp(0, max) as u16,
        green.clamp(0, max) as u16,
        blue.clamp(0, max) as u16,
    ]
}
