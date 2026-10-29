use j2k_jpeg::adapter::{
    assemble_jpeg_baseline_frame, baseline_encode_tables, JpegBaselineHuffmanTable,
    JpegBaselineSampling, JPEG_BASELINE_ZIGZAG,
};
use j2k_jpeg::transcode::{JpegDctCodingMode, JpegDctImage};
use j2k_jpeg::{JpegBackend, JpegEncodeOptions, JpegSubsampling};

use crate::error::WsiError;

mod frame_writer;

/// Largest AC magnitude category an 8-bit baseline Huffman table can code.
const MAX_BASELINE_AC_CATEGORY: u8 = 10;

struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            used: 0,
        }
    }

    fn write_bits(&mut self, code: u16, len: u8) {
        for bit_idx in (0..len).rev() {
            let bit = ((code >> bit_idx) & 1) as u8;
            self.current = (self.current << 1) | bit;
            self.used += 1;
            if self.used == 8 {
                self.push_byte(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
    }

    fn align_with_ones(&mut self) {
        if self.used == 0 {
            return;
        }
        let remaining = 8 - self.used;
        self.current <<= remaining;
        self.current |= (1u8 << remaining) - 1;
        self.push_byte(self.current);
        self.current = 0;
        self.used = 0;
    }

    fn into_bytes(mut self) -> Vec<u8> {
        self.align_with_ones();
        self.bytes
    }

    fn push_byte(&mut self, byte: u8) {
        self.bytes.push(byte);
        if byte == 0xFF {
            self.bytes.push(0x00);
        }
    }
}

/// Re-emit quantized DCT blocks as a baseline JPEG with numeric component IDs,
/// one shared chroma quant table, and an Adobe APP14 marker that states the
/// resolved color transform.
pub(crate) fn encode_baseline_dct_image(image: &JpegDctImage) -> Result<Vec<u8>, WsiError> {
    let BaselineDctLayout {
        sampling,
        adobe_transform,
    } = baseline_dct_layout(image)?;
    let component_count = image.components.len();

    let options = JpegEncodeOptions {
        quality: 90,
        subsampling: if component_count == 1 {
            JpegSubsampling::Gray
        } else {
            JpegSubsampling::Ybr420
        },
        restart_interval: None,
        backend: JpegBackend::Cpu,
    };
    let mut tables = baseline_encode_tables(options)
        .map_err(|err| WsiError::Jpeg(format!("DCT JPEG table setup failed: {err}")))?;
    tables.sampling = sampling;
    tables.q_luma = zigzag_quant_to_natural_u8(&image.components[0].quant_table)?;
    if component_count == 3 {
        if image.components[1].quant_table != image.components[2].quant_table {
            return Err(WsiError::Unsupported {
                reason: "DCT JPEG re-emission supports one shared chroma quant table".into(),
            });
        }
        tables.q_chroma = zigzag_quant_to_natural_u8(&image.components[1].quant_table)?;
    }

    let dc_tables = [&tables.huff_dc_luma, &tables.huff_dc_chroma];
    let ac_tables = [&tables.huff_ac_luma, &tables.huff_ac_chroma];
    let entropy = encode_dct_entropy(image, sampling, dc_tables, ac_tables)?;
    assemble_jpeg_baseline_frame(
        &entropy,
        image.width,
        image.height,
        &tables,
        options,
        JpegBackend::Cpu,
    )
    .map(|encoded| {
        let mut data = encoded.data;
        if let Some(transform) = adobe_transform {
            // Re-emission writes numeric component IDs and drops source APP
            // markers. Preserve the resolved component semantics explicitly,
            // including 4:4:4 YCbCr, whose sampling alone is ambiguous.
            data.splice(
                2..2,
                [
                    0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, transform,
                ],
            );
        }
        data
    })
    .map_err(|err| WsiError::Jpeg(format!("DCT JPEG frame assembly failed: {err}")))
}

/// Source signalling that a coefficient-domain re-emission carries over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DctFrameMarkers {
    /// Complete APPn marker segments (marker, length, payload), such as JFIF
    /// APP0 and Adobe APP14, written right after SOI.
    pub(crate) app_segments: Vec<u8>,
    /// SOF component identifiers in declaration order.
    pub(crate) component_ids: Vec<u8>,
}

/// Re-emit quantized DCT blocks as a baseline JPEG that keeps the source's
/// signalling: `markers.app_segments` follow SOI, SOF and SOS use
/// `markers.component_ids`, and every component keeps its own quant table, so
/// distinct Cb and Cr tables survive. Huffman tables are the JPEG Annex K
/// defaults and no restart interval is written.
pub(crate) fn encode_baseline_dct_image_with_markers(
    image: &JpegDctImage,
    markers: &DctFrameMarkers,
) -> Result<Vec<u8>, WsiError> {
    let BaselineDctLayout { sampling, .. } = baseline_dct_layout(image)?;
    let (width, height) = match (u16::try_from(image.width), u16::try_from(image.height)) {
        (Ok(width), Ok(height)) if width > 0 && height > 0 => (width, height),
        _ => {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "DCT JPEG re-emission requires nonzero u16 dimensions, got {}x{}",
                    image.width, image.height
                ),
            });
        }
    };
    let components = frame_components(image, &markers.component_ids)?;
    let quant_tables = frame_quant_tables(image, &components)?;
    frame_writer::validate_app_segments(&markers.app_segments)?;

    let options = JpegEncodeOptions {
        quality: 90,
        subsampling: if components.len() == 1 {
            JpegSubsampling::Gray
        } else {
            JpegSubsampling::Ybr420
        },
        restart_interval: None,
        backend: JpegBackend::Cpu,
    };
    let tables = baseline_encode_tables(options)
        .map_err(|err| WsiError::Jpeg(format!("DCT JPEG table setup failed: {err}")))?;
    let dc_tables = [&tables.huff_dc_luma, &tables.huff_dc_chroma];
    let ac_tables = [&tables.huff_ac_luma, &tables.huff_ac_chroma];
    let entropy = encode_dct_entropy(image, sampling, dc_tables, ac_tables)?;
    frame_writer::write_baseline_frame(
        &frame_writer::BaselineFrame {
            width,
            height,
            app_segments: &markers.app_segments,
            quant_tables: &quant_tables,
            components: &components,
            dc_tables,
            ac_tables,
        },
        &entropy,
    )
}

struct BaselineDctLayout {
    sampling: JpegBaselineSampling,
    /// Adobe APP14 transform matching the image's resolved color space.
    adobe_transform: Option<u8>,
}

fn baseline_dct_layout(image: &JpegDctImage) -> Result<BaselineDctLayout, WsiError> {
    if image.coding_mode != JpegDctCodingMode::BaselineSequential {
        return Err(WsiError::Unsupported {
            reason: "DCT JPEG re-emission supports baseline sequential input only".into(),
        });
    }
    let component_count = image.components.len();
    if component_count != 1 && component_count != 3 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "DCT JPEG re-emission supports 1 or 3 components, got {component_count}"
            ),
        });
    }
    let adobe_transform = match (component_count, image.color_space) {
        (1, j2k_jpeg::ColorSpace::Grayscale) => None,
        (3, j2k_jpeg::ColorSpace::Rgb) => Some(0),
        (3, j2k_jpeg::ColorSpace::YCbCr) => Some(1),
        _ => {
            return Err(WsiError::Unsupported {
                reason: "DCT JPEG re-emission has inconsistent color components".into(),
            });
        }
    };

    let max_h = image
        .components
        .iter()
        .map(|component| component.h_samp)
        .max()
        .unwrap_or(1);
    let max_v = image
        .components
        .iter()
        .map(|component| component.v_samp)
        .max()
        .unwrap_or(1);
    if max_h == 0 || max_v == 0 {
        return Err(WsiError::Unsupported {
            reason: "DCT JPEG re-emission requires nonzero sampling factors".into(),
        });
    }

    let mut sampling = JpegBaselineSampling {
        components: component_count as u8,
        h: [0; 3],
        v: [0; 3],
        max_h,
        max_v,
    };
    for (idx, component) in image.components.iter().enumerate() {
        if component.component_index != idx {
            return Err(WsiError::Unsupported {
                reason: "DCT JPEG components must be in SOF declaration order".into(),
            });
        }
        sampling.h[idx] = component.h_samp;
        sampling.v[idx] = component.v_samp;
    }
    validate_dct_component_grids(image, sampling)?;
    Ok(BaselineDctLayout {
        sampling,
        adobe_transform,
    })
}

fn frame_components(
    image: &JpegDctImage,
    component_ids: &[u8],
) -> Result<Vec<frame_writer::FrameComponent>, WsiError> {
    if component_ids.len() != image.components.len() {
        return Err(WsiError::Unsupported {
            reason: format!(
                "DCT JPEG re-emission got {} component IDs for {} components",
                component_ids.len(),
                image.components.len()
            ),
        });
    }
    for (idx, id) in component_ids.iter().enumerate() {
        if component_ids[..idx].contains(id) {
            return Err(WsiError::Unsupported {
                reason: format!("DCT JPEG re-emission requires unique component IDs, {id} repeats"),
            });
        }
    }
    let mut distinct_tables: Vec<&[u16; 64]> = Vec::with_capacity(image.components.len());
    let mut components = Vec::with_capacity(image.components.len());
    for (component, &id) in image.components.iter().zip(component_ids) {
        if !(1..=4).contains(&component.h_samp) || !(1..=4).contains(&component.v_samp) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "DCT JPEG re-emission requires sampling factors in 1..=4, component {id} has {}x{}",
                    component.h_samp, component.v_samp
                ),
            });
        }
        let slot = match distinct_tables
            .iter()
            .position(|table| **table == component.quant_table)
        {
            Some(slot) => slot,
            None => {
                distinct_tables.push(&component.quant_table);
                distinct_tables.len() - 1
            }
        };
        components.push(frame_writer::FrameComponent {
            id,
            h_samp: component.h_samp,
            v_samp: component.v_samp,
            quant_slot: slot as u8,
        });
    }
    Ok(components)
}

/// DQT payload tables in zigzag order, indexed by each component's `quant_slot`.
fn frame_quant_tables(
    image: &JpegDctImage,
    components: &[frame_writer::FrameComponent],
) -> Result<Vec<[u8; 64]>, WsiError> {
    let mut tables: Vec<[u8; 64]> = Vec::with_capacity(components.len());
    for (idx, (component, frame_component)) in image.components.iter().zip(components).enumerate() {
        if usize::from(frame_component.quant_slot) < tables.len() {
            continue;
        }
        let mut table = [0u8; 64];
        for (dst, &value) in table.iter_mut().zip(component.quant_table.iter()) {
            *dst = u8::try_from(value)
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| WsiError::Unsupported {
                    reason: format!(
                        "DCT JPEG re-emission requires 8-bit nonzero quant values, component {idx} has {value}"
                    ),
                })?;
        }
        tables.push(table);
    }
    Ok(tables)
}

fn validate_dct_component_grids(
    image: &JpegDctImage,
    sampling: JpegBaselineSampling,
) -> Result<(), WsiError> {
    let mcu_cols = image.width.div_ceil(u32::from(sampling.max_h) * 8);
    let mcu_rows = image.height.div_ceil(u32::from(sampling.max_v) * 8);
    for (idx, component) in image.components.iter().enumerate() {
        let expected_block_cols = mcu_cols * u32::from(sampling.h[idx]);
        let expected_block_rows = mcu_rows * u32::from(sampling.v[idx]);
        let expected_blocks = expected_block_cols
            .checked_mul(expected_block_rows)
            .ok_or_else(|| WsiError::Unsupported {
                reason: "DCT block count overflow".into(),
            })?;
        if component.block_cols != expected_block_cols
            || component.block_rows != expected_block_rows
            || component.quantized_blocks.len() != expected_blocks as usize
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "DCT component {idx} grid is {}x{} blocks with {} blocks, expected {}x{} and {} blocks",
                    component.block_cols,
                    component.block_rows,
                    component.quantized_blocks.len(),
                    expected_block_cols,
                    expected_block_rows,
                    expected_blocks
                ),
            });
        }
    }
    Ok(())
}

fn zigzag_quant_to_natural_u8(quant: &[u16; 64]) -> Result<[u8; 64], WsiError> {
    let mut natural = [0u8; 64];
    for (zigzag_idx, &natural_idx) in JPEG_BASELINE_ZIGZAG.iter().enumerate() {
        natural[usize::from(natural_idx)] =
            u8::try_from(quant[zigzag_idx]).map_err(|_| WsiError::Unsupported {
                reason: "DCT JPEG re-emission supports 8-bit quant tables only".into(),
            })?;
    }
    Ok(natural)
}

fn encode_dct_entropy(
    image: &JpegDctImage,
    sampling: JpegBaselineSampling,
    dc_tables: [&JpegBaselineHuffmanTable; 2],
    ac_tables: [&JpegBaselineHuffmanTable; 2],
) -> Result<Vec<u8>, WsiError> {
    let mcu_cols = image.width.div_ceil(u32::from(sampling.max_h) * 8);
    let mcu_rows = image.height.div_ceil(u32::from(sampling.max_v) * 8);
    let mut writer = BitWriter::new();
    let mut prev_dc = [0i32; 3];

    for mcu_y in 0..mcu_rows {
        for mcu_x in 0..mcu_cols {
            for (component_idx, prev_dc_component) in prev_dc
                .iter_mut()
                .enumerate()
                .take(sampling.components as usize)
            {
                let component = &image.components[component_idx];
                let table_idx = usize::from(component_idx != 0);
                for block_y in 0..sampling.v[component_idx] {
                    for block_x in 0..sampling.h[component_idx] {
                        let source_block_x =
                            mcu_x * u32::from(sampling.h[component_idx]) + u32::from(block_x);
                        let source_block_y =
                            mcu_y * u32::from(sampling.v[component_idx]) + u32::from(block_y);
                        let block_idx =
                            (source_block_y * component.block_cols + source_block_x) as usize;
                        let mut coeffs = [0i32; 64];
                        for (dst, &src) in coeffs
                            .iter_mut()
                            .zip(component.quantized_blocks[block_idx].iter())
                        {
                            *dst = i32::from(src);
                        }
                        encode_block(
                            &coeffs,
                            prev_dc_component,
                            dc_tables[table_idx],
                            ac_tables[table_idx],
                            &mut writer,
                        )?;
                    }
                }
            }
        }
    }
    Ok(writer.into_bytes())
}

fn encode_block(
    coeffs: &[i32; 64],
    prev_dc: &mut i32,
    dc_table: &JpegBaselineHuffmanTable,
    ac_table: &JpegBaselineHuffmanTable,
    writer: &mut BitWriter,
) -> Result<(), WsiError> {
    let diff = coeffs[0] - *prev_dc;
    *prev_dc = coeffs[0];
    let dc_size = magnitude_category(diff);
    write_huffman_symbol(dc_table, dc_size, writer)?;
    if dc_size > 0 {
        writer.write_bits(magnitude_bits(diff, dc_size), dc_size);
    }

    let mut zero_run = 0u8;
    for k in 1..64 {
        let coeff = coeffs[JPEG_BASELINE_ZIGZAG[k] as usize];
        if coeff == 0 {
            zero_run = zero_run.saturating_add(1);
            continue;
        }
        while zero_run >= 16 {
            write_huffman_symbol(ac_table, 0xF0, writer)?;
            zero_run -= 16;
        }
        let size = magnitude_category(coeff);
        if size > MAX_BASELINE_AC_CATEGORY {
            // A larger category would spill into the run-length nibble and
            // silently encode a different symbol.
            return Err(WsiError::Jpeg(format!(
                "DCT JPEG AC coefficient {coeff} exceeds the 8-bit baseline range"
            )));
        }
        let symbol = (zero_run << 4) | size;
        write_huffman_symbol(ac_table, symbol, writer)?;
        writer.write_bits(magnitude_bits(coeff, size), size);
        zero_run = 0;
    }
    if zero_run > 0 {
        write_huffman_symbol(ac_table, 0, writer)?;
    }
    Ok(())
}

fn write_huffman_symbol(
    table: &JpegBaselineHuffmanTable,
    symbol: u8,
    writer: &mut BitWriter,
) -> Result<(), WsiError> {
    let len = table.lens[symbol as usize];
    if len == 0 {
        return Err(WsiError::Jpeg(format!(
            "DCT JPEG entropy symbol has no Huffman code: {symbol}"
        )));
    }
    writer.write_bits(table.codes[symbol as usize], len);
    Ok(())
}

fn magnitude_category(value: i32) -> u8 {
    if value == 0 {
        return 0;
    }
    let mut abs = value.unsigned_abs();
    let mut size = 0u8;
    while abs > 0 {
        size += 1;
        abs >>= 1;
    }
    size
}

fn magnitude_bits(value: i32, size: u8) -> u16 {
    if size == 0 {
        return 0;
    }
    if value >= 0 {
        value as u16
    } else {
        (value + ((1i32 << size) - 1)) as u16
    }
}
