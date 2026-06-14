use signinum_jpeg::adapter::{
    assemble_jpeg_baseline_frame, baseline_encode_tables, JpegBaselineHuffmanTable,
    JpegBaselineSampling, JPEG_BASELINE_ZIGZAG,
};
use signinum_jpeg::transcode::{JpegDctCodingMode, JpegDctImage};
use signinum_jpeg::{JpegBackend, JpegEncodeOptions, JpegSubsampling};

use crate::error::WsiError;

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

pub(super) fn encode_baseline_dct_image(image: &JpegDctImage) -> Result<Vec<u8>, WsiError> {
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
    .map(|encoded| encoded.data)
    .map_err(|err| WsiError::Jpeg(format!("DCT JPEG frame assembly failed: {err}")))
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
