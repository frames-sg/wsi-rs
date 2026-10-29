//! NDPI tile locations: restart intervals of the level's single JPEG, or
//! the whole JPEG when an interval cannot stand for the tile.

use super::super::ndpi_core::ndpi_restart_segments_align_to_rows;
use super::*;
use crate::decode::jpeg::frame::{next_jpeg_segment, JpegFrameInfo};
use crate::decode::jpeg::is_sof_marker;

const EOI: [u8; 2] = [0xFF, 0xD9];

/// What every tile of one NDPI level plane shares.
pub(super) struct NdpiParams {
    /// Framing of restart-interval tiles; `None` when the intervals do not
    /// align to image rows or the header cannot be resized to one tile.
    restart: Option<NdpiRestartFraming>,
    /// The level's whole JPEG as one unit; `Ok(None)` when no standard
    /// stream can be framed for it.
    strip: Result<Option<NdpiStripUnit>, String>,
}

/// How [`TiffPixelReader::read_ndpi_raw_jpeg_tile`] frames a restart
/// interval: the level header with SOF set to the virtual tile and DRI
/// cleared, the interval's entropy bytes, then EOI.
struct NdpiRestartFraming {
    codec: EncodedCodec,
    prefix: Arc<[u8]>,
    suffix: Arc<[u8]>,
    stored: (u32, u32),
    /// The MCU-starts table points at each interval's RST marker, so every
    /// interval after the first starts with one.
    leading_marker: bool,
    /// The table points just past the RST markers, so every interval but
    /// the last ends with the next interval's marker.
    trailing_marker: bool,
    /// The strip ends with EOI, which the last interval carries.
    trailing_eoi: bool,
}

/// The level's JPEG as one stored unit.
struct NdpiStripUnit {
    codec: EncodedCodec,
    framing: StoredFraming,
    prefix: Arc<[u8]>,
    stored: (u32, u32),
    segment: ByteRange,
}

impl TiffPixelReader {
    pub(super) fn ndpi_location_params(
        &self,
        req: &TileRequest,
        source: &TileSource,
    ) -> Result<NdpiParams, WsiError> {
        let (level_w, level_h, vtw, vth) = self.ndpi_whole_level_geometry(
            req.scene,
            req.series,
            req.level,
            req.col,
            req.row,
            "NDPI tile locations expect a WholeLevel tile layout",
        )?;
        match source {
            TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                restart_interval,
                strip_offset,
                strip_byte_count,
                ..
            } => {
                let restart =
                    if ndpi_restart_segments_align_to_rows(level_w, vtw, *restart_interval) {
                        self.ndpi_restart_framing(
                            *ifd_id,
                            jpeg_header,
                            *mcu_starts_tag,
                            *strip_offset,
                            *strip_byte_count,
                            (vtw, vth),
                        )?
                    } else {
                        None
                    };
                Ok(NdpiParams {
                    restart,
                    strip: self.ndpi_strip_unit(
                        *ifd_id,
                        jpeg_header,
                        *strip_offset,
                        *strip_byte_count,
                        (level_w, level_h),
                    ),
                })
            }
            TileSource::NdpiFullDecode {
                ifd_id,
                jpeg_header,
                strip_offset,
                strip_byte_count,
            } => Ok(NdpiParams {
                restart: None,
                strip: self.ndpi_strip_unit(
                    *ifd_id,
                    jpeg_header,
                    *strip_offset,
                    *strip_byte_count,
                    (level_w, level_h),
                ),
            }),
            _ => Err(WsiError::tile_read(req, "source is not an NDPI JPEG level")),
        }
    }

    /// The restart framing of a level, or `None` when the level header cannot
    /// be resized to one virtual tile. Reads the two bytes around the second
    /// interval's start and the strip's last two bytes, once per level, to
    /// learn which side of each RST marker the MCU-starts table points at.
    fn ndpi_restart_framing(
        &self,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        strip_offset: u64,
        strip_byte_count: u64,
        (vtw, vth): (u32, u32),
    ) -> Result<Option<NdpiRestartFraming>, WsiError> {
        let mut prefix = jpeg_header.to_vec();
        disable_jpeg_restart_interval(&mut prefix);
        if patch_jpeg_sof0_dimensions(&mut prefix, vtw, vth).is_err() {
            return Ok(None);
        }
        let Ok(info) = parse_baseline_jpeg_frame_info(&prefix) else {
            return Ok(None);
        };
        let photometric = self.tiff_jpeg_raw_photometric(ifd_id, &prefix, &info)?;

        let starts =
            self.ndpi_mcu_starts(ifd_id, mcu_starts_tag, strip_offset, strip_byte_count)?;
        let read = |offset: u64, len: u64| {
            strip_offset
                .checked_add(offset)
                .ok_or_else(|| WsiError::UnsupportedFormat("NDPI strip offset overflow".into()))
                .and_then(|offset| {
                    self.container
                        .pread(offset, len)
                        .map_err(|err| err.into_wsi_error(self.container.path()))
                })
        };
        let is_restart = |bytes: &[u8]| bytes[0] == 0xFF && (0xD0..=0xD7).contains(&bytes[1]);
        let (leading_marker, trailing_marker) = match starts.as_slice().get(1) {
            Some(&second) if second >= 2 && second.saturating_add(2) <= strip_byte_count => {
                let around = read(second - 2, 4)?;
                let at_marker = is_restart(&around[2..]);
                (at_marker, !at_marker && is_restart(&around[..2]))
            }
            _ => (false, false),
        };
        let trailing_eoi = strip_byte_count >= 2 && read(strip_byte_count - 2, 2)? == EOI;

        Ok(Some(NdpiRestartFraming {
            codec: jpeg_frame_codec(&info, photometric),
            prefix: Arc::from(prefix),
            suffix: Arc::from(&EOI[..]),
            stored: (vtw, vth),
            leading_marker,
            trailing_marker,
            trailing_eoi,
        }))
    }

    pub(super) fn ndpi_tile_location(
        &self,
        req: &TileRequest,
        source: &TileSource,
        params: &NdpiParams,
    ) -> Result<TileLocation, WsiError> {
        self.ndpi_tile_location_kind(req, source, params)
            .map(|(location, _)| location)
    }

    /// Where `req` lies in its level's single JPEG, for tiles no restart
    /// interval serves; `None` when a restart interval does.
    pub(in super::super) fn ndpi_level_jpeg_location(
        &self,
        req: &TileRequest,
    ) -> Result<Option<TileLocation>, WsiError> {
        let source = self.tile_source_for(req)?;
        let params = self.location_params(req, source)?;
        let SourceParams::Ndpi(params) = params.as_ref() else {
            return Err(WsiError::tile_read(req, "NDPI source has tile parameters"));
        };
        let (location, restart) = self.ndpi_tile_location_kind(req, source, params)?;
        Ok((!restart).then_some(location))
    }

    /// The tile's location, and whether a restart interval serves it rather
    /// than the level's whole JPEG.
    fn ndpi_tile_location_kind(
        &self,
        req: &TileRequest,
        source: &TileSource,
        params: &NdpiParams,
    ) -> Result<(TileLocation, bool), WsiError> {
        let (level_w, level_h, vtw, vth) = self.ndpi_whole_level_geometry(
            req.scene,
            req.series,
            req.level,
            req.col,
            req.row,
            "NDPI tile locations expect a WholeLevel tile layout",
        )?;
        let (col, row) = checked_grid_tile(
            req,
            level_w.div_ceil(u64::from(vtw)),
            level_h.div_ceil(u64::from(vth)),
        )?;
        let (x, y) = (col * u64::from(vtw), row * u64::from(vth));
        let width = (level_w - x).min(u64::from(vtw)) as u32;
        let height = (level_h - y).min(u64::from(vth)) as u32;
        let tile_region = PixelRect::new(0, 0, width, height);

        if let (
            TileSource::NdpiJpeg {
                ifd_id,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                strip_offset,
                strip_byte_count,
                ..
            },
            Some(restart),
        ) = (source, &params.restart)
        {
            checked_grid_tile(req, u64::from(*tiles_across), u64::from(*tiles_down))?;
            let strip_key = NdpiStripKey {
                ifd_id: *ifd_id,
                col: col as u32,
                native_row: row as u32,
                scale_denom: 1,
            };
            // An interval the table cannot locate is served by the whole JPEG.
            if let Ok((start, end)) = self.ndpi_restart_segment(
                *ifd_id,
                *mcu_starts_tag,
                *tiles_across,
                *strip_offset,
                *strip_byte_count,
                strip_key,
            )? {
                let first = row * u64::from(*tiles_across) + col == 0;
                let lead = if restart.leading_marker && !first {
                    2
                } else {
                    0
                };
                let trail = if end == *strip_byte_count {
                    if restart.trailing_eoi {
                        2
                    } else {
                        0
                    }
                } else if restart.trailing_marker {
                    2
                } else {
                    0
                };
                let range = (end - start)
                    .checked_sub(lead + trail)
                    .filter(|&length| length > 0)
                    .zip(strip_offset.checked_add(start + lead))
                    .map(|(length, offset)| ByteRange::new(offset, length))
                    .ok_or_else(|| {
                        WsiError::tile_read(
                            req,
                            format!("NDPI restart interval [{start}, {end}) holds no entropy data"),
                        )
                    })?;
                let unit = EncodedUnit::builder(
                    self.locations.path.clone(),
                    restart.codec.clone(),
                    restart.stored.0,
                    restart.stored.1,
                )
                .segment(range)
                .framing(StoredFraming::Prefixed)
                .prefix_suffix(restart.prefix.clone(), restart.suffix.clone())
                .source_region(tile_region)
                .tile_region(tile_region)
                .build(self.locations.file_len)?;
                let stored = StoredTile::new(width, height, vec![unit])?;
                return Ok((TileLocation::Stored(stored), true));
            }
        }

        let strip = match &params.strip {
            Ok(Some(strip)) => strip,
            Ok(None) => {
                return Ok((
                    TileLocation::NotAddressable(NotAddressableReason::CodestreamWindow),
                    false,
                ))
            }
            Err(reason) => return Err(WsiError::tile_read(req, reason.clone())),
        };
        let source_region = PixelRect::new(
            u32::try_from(x).map_err(|_| WsiError::tile_read(req, "NDPI tile x exceeds u32"))?,
            u32::try_from(y).map_err(|_| WsiError::tile_read(req, "NDPI tile y exceeds u32"))?,
            width,
            height,
        );
        let mut builder = EncodedUnit::builder(
            self.locations.path.clone(),
            strip.codec.clone(),
            strip.stored.0,
            strip.stored.1,
        )
        .segment(strip.segment)
        .framing(strip.framing)
        .source_region(source_region)
        .tile_region(tile_region);
        if strip.framing == StoredFraming::Prefixed {
            builder = builder.prefix_suffix(strip.prefix.clone(), Arc::from(&[][..]));
        }
        let unit = builder.build(self.locations.file_len)?;
        let stored = StoredTile::new(width, height, vec![unit])?;
        Ok((TileLocation::Stored(stored), false))
    }
}

impl TiffPixelReader {
    /// The level's whole JPEG strip as one unit. A strip whose SOF states its
    /// size is a standalone JPEG. A zero SOF size (levels past the JPEG size
    /// limit) gets a header stating the level size, restart interval kept, in
    /// front of the strip's entropy data; `Ok(None)` when the level size does
    /// not fit a JPEG header either. Its color is read from the whole header
    /// (Adobe APP14, SOF component ids and sampling) as the decoder reads it.
    fn ndpi_strip_unit(
        &self,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        strip_offset: u64,
        strip_byte_count: u64,
        (level_w, level_h): (u64, u64),
    ) -> Result<Option<NdpiStripUnit>, String> {
        let photometric = |info: &JpegFrameInfo| {
            self.tiff_jpeg_raw_photometric(ifd_id, jpeg_header, info)
                .map_err(|err| err.to_string())
        };
        let (sof_w, sof_h) = sof0_dimensions(jpeg_header)?;
        if sof_w > 0 && sof_h > 0 {
            if level_w > u64::from(sof_w) || level_h > u64::from(sof_h) {
                return Err(format!(
                    "NDPI JPEG frame {sof_w}x{sof_h} is smaller than its {level_w}x{level_h} level"
                ));
            }
            let info =
                parse_baseline_jpeg_frame_info(jpeg_header).map_err(|err| err.to_string())?;
            return Ok(Some(NdpiStripUnit {
                codec: jpeg_frame_codec(&info, photometric(&info)?),
                framing: StoredFraming::Standalone,
                prefix: Arc::from(&[][..]),
                stored: (sof_w, sof_h),
                segment: ByteRange::new(strip_offset, strip_byte_count),
            }));
        }

        let (Ok(width), Ok(height)) = (u32::try_from(level_w), u32::try_from(level_h)) else {
            return Ok(None);
        };
        let mut prefix = jpeg_header.to_vec();
        if patch_jpeg_sof0_dimensions(&mut prefix, width, height).is_err() {
            return Ok(None);
        }
        let info = parse_baseline_jpeg_frame_info(&prefix).map_err(|err| err.to_string())?;
        let header_len = jpeg_header.len() as u64;
        let segment = strip_byte_count
            .checked_sub(header_len)
            .filter(|&entropy| entropy > 0)
            .zip(strip_offset.checked_add(header_len))
            .map(|(entropy, offset)| ByteRange::new(offset, entropy))
            .ok_or_else(|| "NDPI strip holds no entropy data after its JPEG header".to_string())?;
        Ok(Some(NdpiStripUnit {
            codec: jpeg_frame_codec(&info, photometric(&info)?),
            framing: StoredFraming::Prefixed,
            prefix: Arc::from(prefix),
            stored: (width, height),
            segment,
        }))
    }
}

/// The SOF0 frame size of a JPEG header, zero sizes included.
fn sof0_dimensions(header: &[u8]) -> Result<(u32, u32), String> {
    let mut offset = 0;
    while let Some(segment) = next_jpeg_segment(header, offset).map_err(|err| err.to_string())? {
        if segment.marker == 0xC0 {
            let payload = segment.payload;
            if payload.len() < 5 {
                return Err("NDPI JPEG SOF0 segment is truncated".into());
            }
            return Ok((
                u32::from(u16::from_be_bytes([payload[3], payload[4]])),
                u32::from(u16::from_be_bytes([payload[1], payload[2]])),
            ));
        }
        if is_sof_marker(segment.marker) {
            return Err("NDPI tile locations need a Baseline JPEG SOF0 frame".into());
        }
        if segment.marker == 0xDA {
            break;
        }
        offset = segment.end;
    }
    Err("NDPI JPEG header has no SOF0 frame".into())
}

fn jpeg_frame_codec(
    info: &JpegFrameInfo,
    photometric: EncodedTilePhotometricInterpretation,
) -> EncodedCodec {
    EncodedCodec::new(
        Compression::Jpeg,
        info.bits_allocated,
        info.samples_per_pixel,
        photometric,
    )
}
