//! Header inspection of lossless-crop sources: what the coefficient
//! extraction will hold, and which source markers the crop must carry over.

use crate::decode::jpeg::frame::next_jpeg_segment;
use crate::error::WsiError;

/// Baseline JPEG's limit on blocks per interleaved MCU.
const MAX_BLOCKS_PER_MCU: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceComponent {
    pub(super) id: u8,
    pub(super) h_samp: u8,
    pub(super) v_samp: u8,
    /// Zigzag-order table in force at the first scan; `None` when a later
    /// DQT defines it, as progressive streams may.
    pub(super) quant_table: Option<[u16; 64]>,
}

/// Whether a segment is JFIF APP0 or Adobe APP14 color signalling.
fn is_color_app_segment(marker: u8, payload: &[u8]) -> bool {
    match marker {
        0xE0 => payload.starts_with(b"JFIF\0"),
        0xEE => payload.len() >= 12 && payload.starts_with(b"Adobe"),
        _ => false,
    }
}

/// Source markers that decide how decoders interpret the components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ColorSignalling {
    pub(super) jfif: bool,
    pub(super) adobe_transform: Option<u8>,
}

/// The parts of one standalone JPEG header a lossless crop depends on.
#[derive(Debug, Clone)]
pub(super) struct SourceFrame {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) components: Vec<SourceComponent>,
    /// Progressive, or baseline split over several scans: extraction then
    /// also accumulates a 32-bit coefficient plane.
    pub(super) multi_scan: bool,
    pub(super) color: ColorSignalling,
}

impl SourceFrame {
    pub(super) fn parse(data: &[u8]) -> Result<Self, WsiError> {
        if !data.starts_with(&[0xFF, 0xD8]) {
            return Err(WsiError::Jpeg(
                "lossless JPEG crop input does not start with SOI".into(),
            ));
        }
        let mut quant_slots: [Option<[u16; 64]>; 4] = [None; 4];
        let mut sof: Option<SofHeader> = None;
        let mut color = ColorSignalling {
            jfif: false,
            adobe_transform: None,
        };
        let mut offset = 2usize;
        loop {
            let segment = next_jpeg_segment(data, offset)?.ok_or_else(|| {
                WsiError::Jpeg("lossless JPEG crop input ends before its first scan".into())
            })?;
            match segment.marker {
                0xDB => parse_dqt(segment.payload, &mut quant_slots)?,
                0xC0 | 0xC2 => {
                    if sof.is_some() {
                        return Err(WsiError::Jpeg(
                            "lossless JPEG crop input has more than one SOF marker".into(),
                        ));
                    }
                    sof = Some(parse_sof(segment.payload, segment.marker == 0xC2)?);
                }
                0xC1 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCF | 0xDE => {
                    return Err(unsupported_process(segment.marker));
                }
                0xE0 if is_color_app_segment(segment.marker, segment.payload) => color.jfif = true,
                0xEE if is_color_app_segment(segment.marker, segment.payload) => {
                    color.adobe_transform.get_or_insert(segment.payload[11]);
                }
                0xD8 | 0xD9 => {
                    return Err(WsiError::Jpeg(format!(
                        "lossless JPEG crop input has marker FF{:02X} before its first scan",
                        segment.marker
                    )));
                }
                0xDA => {
                    let sof = sof.ok_or_else(|| {
                        WsiError::Jpeg("lossless JPEG crop input has a scan before SOF".into())
                    })?;
                    let scan_components = segment.payload.first().copied().unwrap_or(0);
                    let multi_scan =
                        sof.progressive || usize::from(scan_components) < sof.components.len();
                    let components = sof
                        .components
                        .into_iter()
                        .map(|component| SourceComponent {
                            id: component.id,
                            h_samp: component.h_samp,
                            v_samp: component.v_samp,
                            quant_table: quant_slots[usize::from(component.quant_slot)],
                        })
                        .collect::<Vec<_>>();
                    validate_quant_precision(&components)?;
                    return Ok(Self {
                        width: sof.width,
                        height: sof.height,
                        components,
                        multi_scan,
                        color,
                    });
                }
                _ => {}
            }
            offset = segment.end;
        }
    }

    /// Copies of `data`'s complete JFIF APP0 and Adobe APP14 segments, in
    /// source order, for the crop to carry over. [`Self::parse`] only reads
    /// them, so a source returned whole never holds a copy.
    pub(super) fn color_app_segments(data: &[u8]) -> Result<Vec<u8>, WsiError> {
        let mut segments = Vec::new();
        let mut offset = 2usize;
        while let Some(segment) = next_jpeg_segment(data, offset)? {
            if segment.marker == 0xDA {
                break;
            }
            if is_color_app_segment(segment.marker, segment.payload) {
                segments.extend_from_slice(&data[segment.start..segment.end]);
            }
            offset = segment.end;
        }
        Ok(segments)
    }

    /// MCU width and height in pixels.
    pub(super) fn mcu_dimensions(&self) -> (u32, u32) {
        let max_h = self.components.iter().map(|c| c.h_samp).max().unwrap_or(1);
        let max_v = self.components.iter().map(|c| c.v_samp).max().unwrap_or(1);
        (u32::from(max_h) * 8, u32::from(max_v) * 8)
    }

    /// Number of 8x8 blocks in one MCU across every component.
    pub(super) fn blocks_per_mcu(&self) -> u32 {
        self.components
            .iter()
            .map(|c| u32::from(c.h_samp) * u32::from(c.v_samp))
            .sum()
    }

    /// Bytes of coefficient planes extracting this whole frame holds at peak:
    /// quantized and dequantized `i16` blocks, plus the `i32` accumulation
    /// plane of progressive and multi-scan streams.
    pub(super) fn extraction_bytes(&self) -> Option<u64> {
        let (mcu_width, mcu_height) = self.mcu_dimensions();
        let blocks = u64::from(self.width.div_ceil(mcu_width))
            .checked_mul(u64::from(self.height.div_ceil(mcu_height)))?
            .checked_mul(u64::from(self.blocks_per_mcu()))?;
        let bytes_per_block = 2 * 64 * 2 + if self.multi_scan { 64 * 4 } else { 0 };
        blocks.checked_mul(bytes_per_block)
    }
}

fn unsupported_process(marker: u8) -> WsiError {
    let process = match marker {
        0xC1 => "extended sequential (SOF1) JPEG",
        0xC3 => "lossless (SOF3) JPEG",
        0xC5..=0xC7 | 0xDE => "hierarchical JPEG",
        0xC9..=0xCC => "arithmetic-coded JPEG",
        _ => "hierarchical arithmetic-coded JPEG",
    };
    WsiError::Unsupported {
        reason: format!(
            "lossless JPEG crop supports baseline and progressive Huffman JPEG, got {process}"
        ),
    }
}

struct SofComponent {
    id: u8,
    h_samp: u8,
    v_samp: u8,
    quant_slot: u8,
}

struct SofHeader {
    width: u32,
    height: u32,
    components: Vec<SofComponent>,
    progressive: bool,
}

fn parse_sof(payload: &[u8], progressive: bool) -> Result<SofHeader, WsiError> {
    let [precision, h0, h1, w0, w1, count, table @ ..] = payload else {
        return Err(WsiError::Jpeg(
            "lossless JPEG crop SOF segment is truncated".into(),
        ));
    };
    if *precision != 8 {
        return Err(WsiError::Unsupported {
            reason: format!("lossless JPEG crop requires 8-bit samples, got {precision}-bit"),
        });
    }
    let height = u32::from(u16::from_be_bytes([*h0, *h1]));
    let width = u32::from(u16::from_be_bytes([*w0, *w1]));
    if width == 0 || height == 0 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "lossless JPEG crop requires nonzero SOF dimensions, got {width}x{height}"
            ),
        });
    }
    match *count {
        1 | 3 => {}
        4 => {
            return Err(WsiError::Unsupported {
                reason: "lossless JPEG crop supports 1 or 3 components, got a 4-component (CMYK/YCCK) JPEG".into(),
            });
        }
        other => {
            return Err(WsiError::Unsupported {
                reason: format!("lossless JPEG crop supports 1 or 3 components, got {other}"),
            });
        }
    }
    let entries = table.get(..usize::from(*count) * 3).ok_or_else(|| {
        WsiError::Jpeg("lossless JPEG crop SOF component table is truncated".into())
    })?;
    let mut components: Vec<SofComponent> = Vec::with_capacity(usize::from(*count));
    for &[id, sampling, quant_slot] in entries.as_chunks::<3>().0 {
        let (h_samp, v_samp) = (sampling >> 4, sampling & 0x0F);
        if !(1..=4).contains(&h_samp) || !(1..=4).contains(&v_samp) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "lossless JPEG crop requires sampling factors in 1..=4, component {id} has {h_samp}x{v_samp}"
                ),
            });
        }
        if quant_slot > 3 {
            return Err(WsiError::Jpeg(format!(
                "lossless JPEG crop component {id} selects quant table {quant_slot}, beyond 0..=3"
            )));
        }
        if components.iter().any(|other| other.id == id) {
            return Err(WsiError::Unsupported {
                reason: format!("lossless JPEG crop requires unique component IDs, {id} repeats"),
            });
        }
        components.push(SofComponent {
            id,
            h_samp,
            v_samp,
            quant_slot,
        });
    }
    if let [component] = &components[..] {
        if (component.h_samp, component.v_samp) != (1, 1) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "lossless JPEG crop requires 1x1 sampling for single-component JPEG, got {}x{}",
                    component.h_samp, component.v_samp
                ),
            });
        }
    }
    let blocks_per_mcu: u32 = components
        .iter()
        .map(|c| u32::from(c.h_samp) * u32::from(c.v_samp))
        .sum();
    if blocks_per_mcu > MAX_BLOCKS_PER_MCU {
        return Err(WsiError::Unsupported {
            reason: format!(
                "lossless JPEG crop requires at most {MAX_BLOCKS_PER_MCU} blocks per MCU, got {blocks_per_mcu}"
            ),
        });
    }
    Ok(SofHeader {
        width,
        height,
        components,
        progressive,
    })
}

fn parse_dqt(payload: &[u8], slots: &mut [Option<[u16; 64]>; 4]) -> Result<(), WsiError> {
    let mut rest = payload;
    while let Some((&precision_and_id, body)) = rest.split_first() {
        let (precision, id) = (precision_and_id >> 4, usize::from(precision_and_id & 0x0F));
        let value_bytes = match precision {
            0 => 1,
            1 => 2,
            other => {
                return Err(WsiError::Jpeg(format!(
                    "lossless JPEG crop DQT has invalid precision {other}"
                )));
            }
        };
        let slot = slots.get_mut(id).ok_or_else(|| {
            WsiError::Jpeg(format!(
                "lossless JPEG crop DQT defines table {id}, beyond 0..=3"
            ))
        })?;
        let table_len = 64 * value_bytes;
        let values = body
            .get(..table_len)
            .ok_or_else(|| WsiError::Jpeg("lossless JPEG crop DQT segment is truncated".into()))?;
        let mut table = [0u16; 64];
        for (idx, dst) in table.iter_mut().enumerate() {
            *dst = if value_bytes == 1 {
                u16::from(values[idx])
            } else {
                u16::from_be_bytes([values[2 * idx], values[2 * idx + 1]])
            };
        }
        *slot = Some(table);
        rest = &body[table_len..];
    }
    Ok(())
}

fn validate_quant_precision(components: &[SourceComponent]) -> Result<(), WsiError> {
    for component in components {
        if let Some(value) = component
            .quant_table
            .iter()
            .flatten()
            .find(|value| **value == 0 || **value > 255)
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "lossless JPEG crop re-emits baseline 8-bit quant tables, component {} has quant value {value}",
                    component.id
                ),
            });
        }
    }
    Ok(())
}
