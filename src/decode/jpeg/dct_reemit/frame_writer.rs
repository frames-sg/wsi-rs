//! Baseline frame assembly for re-emission that keeps source component IDs,
//! per-component quant tables, and APP markers.

use j2k_jpeg::adapter::JpegBaselineHuffmanTable;

use crate::decode::jpeg::frame::next_jpeg_segment;
use crate::error::WsiError;

pub(super) struct FrameComponent {
    pub(super) id: u8,
    pub(super) h_samp: u8,
    pub(super) v_samp: u8,
    /// Index into [`BaselineFrame::quant_tables`], written as the SOF `Tq`.
    pub(super) quant_slot: u8,
}

pub(super) struct BaselineFrame<'a> {
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) app_segments: &'a [u8],
    /// DQT tables in zigzag order; the index is the table ID.
    pub(super) quant_tables: &'a [[u8; 64]],
    pub(super) components: &'a [FrameComponent],
    /// Luma (component 0) and chroma tables, the same pair the entropy coder used.
    pub(super) dc_tables: [&'a JpegBaselineHuffmanTable; 2],
    pub(super) ac_tables: [&'a JpegBaselineHuffmanTable; 2],
}

/// Reject anything but a run of complete APPn segments, so copied source
/// markers cannot smuggle tables or scans into the re-emitted header.
pub(super) fn validate_app_segments(segments: &[u8]) -> Result<(), WsiError> {
    let mut offset = 0usize;
    while offset < segments.len() {
        let segment = next_jpeg_segment(segments, offset)?
            .filter(|segment| segment.start == offset && (0xE0..=0xEF).contains(&segment.marker))
            .ok_or_else(|| WsiError::Unsupported {
                reason: "DCT JPEG re-emission markers must be complete APPn segments".into(),
            })?;
        offset = segment.end;
    }
    Ok(())
}

pub(super) fn write_baseline_frame(
    frame: &BaselineFrame<'_>,
    entropy: &[u8],
) -> Result<Vec<u8>, WsiError> {
    let component_count = u8::try_from(frame.components.len())
        .ok()
        .filter(|count| *count == 1 || *count == 3)
        .ok_or_else(|| WsiError::Unsupported {
            reason: format!(
                "DCT JPEG re-emission supports 1 or 3 components, got {}",
                frame.components.len()
            ),
        })?;

    let mut out = Vec::with_capacity(entropy.len().saturating_add(1024));
    out.extend_from_slice(&[0xFF, 0xD8]);
    out.extend_from_slice(frame.app_segments);

    let mut dqt = Vec::with_capacity(frame.quant_tables.len() * 65);
    for (id, table) in frame.quant_tables.iter().enumerate() {
        dqt.push(id as u8);
        dqt.extend_from_slice(table);
    }
    write_segment(&mut out, 0xDB, &dqt)?;

    let mut sof = Vec::with_capacity(6 + frame.components.len() * 3);
    sof.push(8);
    sof.extend_from_slice(&frame.height.to_be_bytes());
    sof.extend_from_slice(&frame.width.to_be_bytes());
    sof.push(component_count);
    for component in frame.components {
        sof.extend_from_slice(&[
            component.id,
            (component.h_samp << 4) | component.v_samp,
            component.quant_slot,
        ]);
    }
    write_segment(&mut out, 0xC0, &sof)?;

    let mut dht = Vec::new();
    append_huffman_table(&mut dht, 0, 0, frame.dc_tables[0])?;
    append_huffman_table(&mut dht, 1, 0, frame.ac_tables[0])?;
    if component_count > 1 {
        append_huffman_table(&mut dht, 0, 1, frame.dc_tables[1])?;
        append_huffman_table(&mut dht, 1, 1, frame.ac_tables[1])?;
    }
    write_segment(&mut out, 0xC4, &dht)?;

    let mut sos = Vec::with_capacity(4 + frame.components.len() * 2);
    sos.push(component_count);
    for (idx, component) in frame.components.iter().enumerate() {
        sos.extend_from_slice(&[component.id, if idx == 0 { 0x00 } else { 0x11 }]);
    }
    sos.extend_from_slice(&[0, 63, 0]);
    write_segment(&mut out, 0xDA, &sos)?;

    out.extend_from_slice(entropy);
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

fn write_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) -> Result<(), WsiError> {
    let len = payload
        .len()
        .checked_add(2)
        .and_then(|len| u16::try_from(len).ok())
        .ok_or_else(|| WsiError::Unsupported {
            reason: format!("DCT JPEG marker FF{marker:02X} payload exceeds a JPEG segment"),
        })?;
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// Append one DHT table derived from the code lengths the entropy coder used.
/// The tables are canonical, so ordering symbols by (length, code) restores
/// the HUFFVAL order a decoder rebuilds the same codes from.
fn append_huffman_table(
    out: &mut Vec<u8>,
    class: u8,
    id: u8,
    table: &JpegBaselineHuffmanTable,
) -> Result<(), WsiError> {
    let mut symbols = table
        .lens
        .iter()
        .zip(table.codes.iter())
        .enumerate()
        .filter(|(_, (&len, _))| len > 0)
        .map(|(symbol, (&len, &code))| (len, code, symbol as u8))
        .collect::<Vec<_>>();
    symbols.sort_unstable();
    let mut bits = [0u8; 16];
    for &(len, _, _) in &symbols {
        let count = bits
            .get_mut(usize::from(len) - 1)
            .ok_or_else(|| WsiError::Jpeg(format!("DCT JPEG Huffman code length {len} > 16")))?;
        *count = count.checked_add(1).ok_or_else(|| {
            WsiError::Jpeg(format!(
                "DCT JPEG Huffman table has over 255 codes of length {len}"
            ))
        })?;
    }
    out.push((class << 4) | id);
    out.extend_from_slice(&bits);
    out.extend(symbols.iter().map(|&(_, _, symbol)| symbol));
    Ok(())
}
