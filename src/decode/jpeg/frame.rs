//! Format-agnostic JPEG marker helpers: color hints, frame inspection,
//! abbreviated-stream table merging, and in-place header patches.
//!
//! j2k-jpeg walks the marker segments ([`j2k_jpeg::iter_segments`]), parses
//! restart intervals, and assembles abbreviated TIFF tiles with their
//! `JPEGTables`. What stays here is WSI policy on top of those: which color
//! markers override a container's tags, which frames raw passthrough hands
//! on undecoded, and in-place patches of headers this crate owns.

use std::borrow::Cow;
use std::ops::Range;

use j2k_jpeg::{
    is_sof_marker, iter_segments, parse_dri, prepare_tiff_jpeg_tile, JpegError, JpegSegment,
    JpegTilePrepareOptions, PreparedJpeg,
};

use crate::core::types::EncodedTilePhotometricInterpretation;
use crate::error::WsiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JpegBitstreamColorHint {
    Rgb,
    RgbComponentIds012,
    YCbCr,
    Unknown,
}

/// The marker segments of `data` before its first scan, as j2k-jpeg walks
/// them: SOI, then every segment up to but excluding the first SOS. The walk
/// ends early at EOI and where the stream stops or is not well formed.
pub(crate) fn jpeg_header_segments(data: &[u8]) -> impl Iterator<Item = JpegSegment<'_>> {
    iter_segments(data)
        .map_while(Result::ok)
        .take_while(|segment| segment.marker != 0xDA)
}

/// The first SOF segment before `data`'s first scan.
pub(crate) fn jpeg_sof_segment(data: &[u8]) -> Option<JpegSegment<'_>> {
    jpeg_header_segments(data).find(|segment| is_sof_marker(segment.marker))
}

/// Runs of stray bytes skipped in one header. A stream with more is left to
/// j2k-jpeg's walker, which refuses it.
const MAX_STRAY_HEADER_RUNS: usize = 16;

/// `data` as j2k-jpeg's marker walker reads it up to the first scan: `data`
/// itself when it is well formed there, otherwise a copy without the bytes
/// between marker segments that the walker refuses and libjpeg-family
/// decoders skip. Raw passthrough hands such a stream on as stored, so its
/// headers must still be readable. Offsets into a copy are not offsets into
/// `data`.
fn walkable_jpeg(data: &[u8]) -> Cow<'_, [u8]> {
    let mut stream = Cow::Borrowed(data);
    for _ in 0..MAX_STRAY_HEADER_RUNS {
        let Some(stray) = stray_header_bytes(&stream) else {
            break;
        };
        stream.to_mut().drain(stray);
    }
    stream
}

/// The first bytes before `data`'s first scan that lie outside its marker
/// segments: a run without a `0xFF` marker prefix, or a stuffed `FF 00`.
fn stray_header_bytes(data: &[u8]) -> Option<Range<usize>> {
    for segment in iter_segments(data) {
        match segment {
            Ok(segment) if segment.marker == 0xDA => return None,
            Ok(_) => {}
            Err(JpegError::InvalidMarker { offset, .. }) => {
                let rest = data.get(offset..)?;
                let len = match rest {
                    [0xFF, 0x00, ..] => 2,
                    [0xFF, ..] => return None,
                    _ => rest
                        .iter()
                        .position(|&byte| byte == 0xFF)
                        .unwrap_or(rest.len()),
                };
                return (len > 0).then(|| offset..offset + len);
            }
            Err(_) => return None,
        }
    }
    None
}

/// Whether the marker segments of `head`, the leading bytes of a JPEG, reach
/// a whole SOS header.
pub(crate) fn jpeg_header_reaches_scan(head: &[u8]) -> bool {
    iter_segments(&walkable_jpeg(head))
        .map_while(Result::ok)
        .any(|segment| segment.marker == 0xDA)
}

pub(crate) fn jpeg_bitstream_color_hint(
    data: &[u8],
    tables: Option<&[u8]>,
) -> JpegBitstreamColorHint {
    tables
        .map(jpeg_segment_color_hint)
        .filter(|hint| *hint != JpegBitstreamColorHint::Unknown)
        .unwrap_or_else(|| jpeg_segment_color_hint(data))
}

/// The color the first Adobe APP14 or SOF segment of `data` signals.
pub(crate) fn jpeg_segment_color_hint(data: &[u8]) -> JpegBitstreamColorHint {
    let data = walkable_jpeg(data);
    for segment in jpeg_header_segments(&data) {
        match segment.marker {
            0xEE if segment.payload.len() >= 12 && segment.payload.starts_with(b"Adobe") => {
                return match segment.payload[11] {
                    0 => JpegBitstreamColorHint::Rgb,
                    1 => JpegBitstreamColorHint::YCbCr,
                    _ => JpegBitstreamColorHint::Unknown,
                };
            }
            marker if is_sof_marker(marker) => return jpeg_sof_color_hint(segment.payload),
            _ => {}
        }
    }
    JpegBitstreamColorHint::Unknown
}

/// The color a three-component SOF payload signals by its component IDs and
/// sampling factors. It reads the payload as stored, so a frame j2k-jpeg does
/// not parse, such as an NDPI header stating a zero size, still gives a hint.
pub(crate) fn jpeg_sof_color_hint(payload: &[u8]) -> JpegBitstreamColorHint {
    if payload.len() < 6 {
        return JpegBitstreamColorHint::Unknown;
    }
    let component_count = payload[5] as usize;
    if component_count != 3 || payload.len() < 6 + component_count * 3 {
        return JpegBitstreamColorHint::Unknown;
    }

    let mut ids = [0u8; 3];
    let mut sampling = [(0u8, 0u8); 3];
    for component in 0..component_count {
        let base = 6 + component * 3;
        ids[component] = payload[base];
        sampling[component] = (payload[base + 1] >> 4, payload[base + 1] & 0x0F);
    }

    let first_component_subsampled = sampling[0].0 > sampling[1].0
        || sampling[0].0 > sampling[2].0
        || sampling[0].1 > sampling[1].1
        || sampling[0].1 > sampling[2].1;
    if first_component_subsampled {
        return JpegBitstreamColorHint::YCbCr;
    }

    if ids == *b"RGB" {
        return JpegBitstreamColorHint::Rgb;
    }
    if ids == [0, 1, 2] {
        return JpegBitstreamColorHint::RgbComponentIds012;
    }

    JpegBitstreamColorHint::Unknown
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct JpegFrameInfo {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bits_allocated: u16,
    pub(crate) samples_per_pixel: u16,
    pub(crate) photometric_interpretation: EncodedTilePhotometricInterpretation,
}

/// `tile_data` as a standalone baseline JPEG and its frame: unchanged when
/// its first scan's tables are all defined, otherwise with `jpeg_tables` (a
/// TIFF `JPEGTables` stream) merged in.
pub(crate) fn standalone_jpeg_frame_owned(
    tile_data: Vec<u8>,
    jpeg_tables: Option<&[u8]>,
) -> Result<(Vec<u8>, JpegFrameInfo), WsiError> {
    validate_standalone_jpeg_payload(&tile_data)?;
    let info = parse_baseline_jpeg_frame_info(&tile_data)?;
    Ok((merge_jpeg_tables(tile_data, jpeg_tables)?, info))
}

/// `tile` with the DQT/DHT tables its first scan references but does not
/// define merged in from `tables` (a TIFF `JPEGTables` stream). For baseline
/// tiles the bytes equal [`standalone_jpeg_frame_owned`]'s frame; unlike it,
/// progressive and other Huffman frame types are accepted, for callers that
/// validate the frame themselves.
pub(crate) fn merge_missing_jpeg_tables(
    tile: Vec<u8>,
    tables: Option<&[u8]>,
) -> Result<Vec<u8>, WsiError> {
    validate_standalone_jpeg_payload(&tile)?;
    merge_jpeg_tables(tile, tables)
}

/// The kinds of shared table segments offered to an abbreviated tile, fewest
/// first: only DQT, only DHT, then both.
const TABLE_KINDS: [&[u8]; 3] = [&[0xDB], &[0xC4], &[0xDB, 0xC4]];

/// j2k-jpeg's TIFF tile assembly ([`prepare_tiff_jpeg_tile`]) decides whether
/// a tile's first scan resolves its coding tables and puts shared segments in
/// front of the tile's own, so tile-local definitions replace shared tables
/// with the same IDs (T.81). A complete tile is returned as stored, without
/// a copy.
///
/// j2k-jpeg merges every segment of the tables it is given. A tile that
/// defines one kind of table itself keeps the bytes it has always had by
/// being offered only the kind it lacks: the first of [`TABLE_KINDS`] that
/// completes it is merged.
fn merge_jpeg_tables(tile: Vec<u8>, tables: Option<&[u8]>) -> Result<Vec<u8>, WsiError> {
    let unsupported = |reason: String| Err(WsiError::Unsupported { reason });
    let options = JpegTilePrepareOptions::default();
    let stream = walkable_jpeg(&tile);
    match prepare_tiff_jpeg_tile(&stream, None, options) {
        // Complete. Its tables are too when only its zero frame size, which
        // j2k-jpeg checks after them, stops the assembly.
        Ok(_) | Err(JpegError::ExpectedDimensionsRequired { .. }) => {
            drop(stream);
            return Ok(tile);
        }
        Err(JpegError::InvalidJpegAssembly { .. }) => {}
        Err(err) => {
            return unsupported(format!(
                "JPEG passthrough could not prepare the tile: {err}"
            ))
        }
    }
    let Some(tables) = tables else {
        return unsupported(
            "JPEG passthrough tile is missing table segments and no JPEGTables are available"
                .into(),
        );
    };
    // A tile that defines no table of either kind needs both.
    let defines = |marker| jpeg_header_segments(&stream).any(|segment| segment.marker == marker);
    let skipped = if defines(0xDB) || defines(0xC4) { 0 } else { 2 };
    for kinds in &TABLE_KINDS[skipped..] {
        let mut offered = vec![0xFF, 0xD8];
        for segment in jpeg_header_segments(tables) {
            if kinds.contains(&segment.marker) {
                let end = segment.payload_offset + segment.payload.len();
                offered.extend_from_slice(&tables[segment.marker_offset..end]);
            }
        }
        offered.extend_from_slice(&[0xFF, 0xD9]);
        match prepare_tiff_jpeg_tile(&stream, Some(&offered), options) {
            Ok(PreparedJpeg::Owned(merged)) => return Ok(merged),
            Ok(PreparedJpeg::Borrowed(_)) | Err(JpegError::InvalidJpegAssembly { .. }) => {}
            Err(err) => {
                return unsupported(format!(
                    "JPEG passthrough could not prepare the tile: {err}"
                ))
            }
        }
    }
    unsupported("JPEG passthrough could not rebuild required DQT/DHT table segments".into())
}

pub(crate) fn validate_standalone_jpeg_payload(tile_data: &[u8]) -> Result<(), WsiError> {
    if !tile_data.starts_with(&[0xFF, 0xD8]) {
        return Err(WsiError::Unsupported {
            reason: "JPEG passthrough requires tile payloads to start with SOI".into(),
        });
    }
    if !tile_data.ends_with(&[0xFF, 0xD9]) {
        return Err(WsiError::Unsupported {
            reason: "JPEG passthrough requires tile payloads to end with EOI".into(),
        });
    }
    Ok(())
}

/// The frame raw passthrough takes from `data`'s headers: every SOF before
/// the first scan must be baseline SOF0 with a nonzero 8-bit size and one or
/// three components, and the last one decides.
pub(crate) fn parse_baseline_jpeg_frame_info(data: &[u8]) -> Result<JpegFrameInfo, WsiError> {
    let data = walkable_jpeg(data);
    let mut info = None;
    for segment in jpeg_header_segments(&data) {
        match segment.marker {
            0xC0 => info = Some(jpeg_sof_frame_info(segment.payload)?),
            marker if is_sof_marker(marker) => {
                return Err(WsiError::Unsupported {
                    reason: "JPEG passthrough only supports Baseline JPEG SOF0 frames".into(),
                });
            }
            _ => {}
        }
    }
    info.ok_or_else(|| WsiError::Unsupported {
        reason: "JPEG passthrough could not find a Baseline JPEG SOF0 marker".into(),
    })
}

/// The frame of `data`'s first SOF marker of any coding process, when it
/// states a nonzero 8-bit size with one or three components.
pub(crate) fn parse_jpeg_frame_info(data: &[u8]) -> Option<JpegFrameInfo> {
    let data = walkable_jpeg(data);
    jpeg_sof_frame_info(jpeg_sof_segment(&data)?.payload).ok()
}

/// The frame a SOF payload states, when it is a nonzero 8-bit size with one
/// or three components.
///
/// The payload's fixed fields are read as stored rather than through
/// [`j2k_jpeg::parse_sof_info`], which describes frames j2k-jpeg decodes and
/// so refuses ones raw passthrough hands on undecoded: sizes of 65501 to
/// 65535 pixels, which NDPI levels reach, and coding processes other than
/// the Huffman ones it implements.
fn jpeg_sof_frame_info(payload: &[u8]) -> Result<JpegFrameInfo, WsiError> {
    let unsupported = |reason: String| Err(WsiError::Unsupported { reason });
    let (Some((width, height)), Some(&components)) = (jpeg_sof_dimensions(payload), payload.get(5))
    else {
        return unsupported("JPEG SOF segment is truncated".into());
    };
    let precision = payload[0];
    if precision != 8 {
        return unsupported(format!(
            "JPEG passthrough requires 8-bit JPEG, got {precision}-bit"
        ));
    }
    if width == 0 || height == 0 {
        return unsupported("JPEG passthrough requires nonzero SOF dimensions".into());
    }
    if payload.len() < 6 + usize::from(components) * 3 {
        return unsupported("JPEG SOF component table is truncated".into());
    }
    let photometric_interpretation = match components {
        1 => EncodedTilePhotometricInterpretation::Monochrome2,
        3 => match jpeg_sof_color_hint(payload) {
            JpegBitstreamColorHint::Rgb => EncodedTilePhotometricInterpretation::Rgb,
            _ => EncodedTilePhotometricInterpretation::YbrFull422,
        },
        _ => {
            return unsupported(format!(
                "JPEG passthrough supports 1 or 3 components, got {components}"
            ));
        }
    };
    Ok(JpegFrameInfo {
        width,
        height,
        bits_allocated: 8,
        samples_per_pixel: u16::from(components),
        photometric_interpretation,
    })
}

/// The (width, height) a SOF payload states, zero sizes included.
pub(crate) fn jpeg_sof_dimensions(payload: &[u8]) -> Option<(u32, u32)> {
    let [_, h0, h1, w0, w1, ..] = payload else {
        return None;
    };
    Some((
        u32::from(u16::from_be_bytes([*w0, *w1])),
        u32::from(u16::from_be_bytes([*h0, *h1])),
    ))
}

pub(crate) fn strip_leading_restart_marker(segment: &[u8]) -> &[u8] {
    if segment.len() >= 2 && segment[0] == 0xFF && (0xD0..=0xD7).contains(&segment[1]) {
        &segment[2..]
    } else {
        segment
    }
}

pub(crate) fn strip_trailing_restart_marker(segment: &[u8]) -> &[u8] {
    if segment.len() >= 2 {
        let tail = &segment[segment.len() - 2..];
        if tail[0] == 0xFF && (0xD0..=0xD7).contains(&tail[1]) {
            return &segment[..segment.len() - 2];
        }
    }
    segment
}

pub(crate) fn strip_trailing_eoi_marker(segment: &[u8]) -> &[u8] {
    if segment.len() >= 2 && segment[segment.len() - 2..] == [0xFF, 0xD9] {
        &segment[..segment.len() - 2]
    } else {
        segment
    }
}

/// Zero the restart interval `header` defines before its first scan, so a
/// stream that holds one interval's entropy data decodes without restart
/// markers. A header without a nonzero DRI is left as it is.
pub(crate) fn disable_jpeg_restart_interval(header: &mut [u8]) {
    let interval = jpeg_header_segments(header)
        .find(|segment| segment.marker == 0xDD)
        .filter(|segment| matches!(parse_dri(segment.payload), Ok(Some(_))))
        .map(|segment| segment.payload_offset);
    if let Some(offset) = interval {
        header[offset..offset + 2].fill(0);
    }
}

/// Overwrite the size the baseline SOF0 of `data` states, in place.
pub(crate) fn patch_jpeg_sof0_dimensions(
    data: &mut [u8],
    width: u32,
    height: u32,
) -> Result<(), WsiError> {
    let unsupported = |reason: String| Err(WsiError::Unsupported { reason });
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        return unsupported(format!(
            "NDPI JPEG passthrough requires u16 SOF dimensions, got {width}x{height}"
        ));
    }
    let sof = jpeg_sof_segment(data)
        .map(|segment| (segment.marker, segment.marker_offset, segment.payload.len()));
    match sof {
        Some((0xC0, sof_offset, payload_len)) if payload_len >= 5 => {
            super::set_sof_dimensions(data, sof_offset, width as u16, height as u16);
            Ok(())
        }
        Some((0xC0, ..)) => unsupported("NDPI JPEG passthrough SOF0 segment is truncated".into()),
        Some(_) => {
            unsupported("NDPI JPEG passthrough only supports Baseline JPEG SOF0 frames".into())
        }
        None => {
            unsupported("NDPI JPEG passthrough could not find a Baseline JPEG SOF0 marker".into())
        }
    }
}
