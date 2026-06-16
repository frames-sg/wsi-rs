use super::model::{RawReadWindow, ZviCompression, ZviPlane, ZviSlide};
use super::*;

impl ZviSlide {
    pub(super) fn read_plane_window(
        &self,
        plane_index: usize,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<CpuTile, WsiError> {
        let plane = &self.planes[plane_index];
        if x > plane.width
            || y > plane.height
            || x.saturating_add(w) > plane.width
            || y.saturating_add(h) > plane.height
        {
            return Err(WsiError::TileRead {
                col: 0,
                row: 0,
                level: 0u32,
                reason: "ZVI plane window out of bounds".into(),
            });
        }

        match plane.compression {
            ZviCompression::Raw => self.read_raw_plane_window(plane, x, y, w, h),
            ZviCompression::Zlib => self.read_zlib_plane_window(plane, x, y, w, h),
            ZviCompression::Jpeg => self.read_jpeg_plane_window(plane, x, y, w, h),
        }
    }

    fn read_raw_plane_window(
        &self,
        plane: &ZviPlane,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<CpuTile, WsiError> {
        match plane.bytes_per_sample {
            1 => {
                let mut samples = vec![0u8; w as usize * h as usize];
                self.read_raw_rows(
                    plane,
                    RawReadWindow {
                        x,
                        y,
                        width: w,
                        height: h,
                        bytes_per_sample: 1,
                    },
                    &mut samples,
                )?;
                CpuTile::new(
                    w,
                    h,
                    1,
                    ColorSpace::Grayscale,
                    CpuTileLayout::Interleaved,
                    CpuTileData::u8(samples),
                )
            }
            2 => {
                let mut row_bytes = vec![0u8; w as usize * 2];
                let mut samples = vec![0u16; w as usize * h as usize];
                let mut compound = self.compound.lock().unwrap_or_else(|e| e.into_inner());
                let mut stream = compound.open_stream(&plane.stream_path)?;
                for row in 0..h {
                    let src_offset = plane
                        .payload_offset
                        .checked_add(
                            (u64::from(y + row) * u64::from(plane.width) + u64::from(x)) * 2,
                        )
                        .ok_or_else(|| {
                            WsiError::DisplayConversion("ZVI raw row offset overflow".into())
                        })?;
                    stream.seek(SeekFrom::Start(src_offset))?;
                    stream.read_exact(&mut row_bytes)?;
                    let dst = row as usize * w as usize;
                    for (slot, bytes) in samples[dst..dst + w as usize]
                        .iter_mut()
                        .zip(row_bytes.chunks_exact(2))
                    {
                        *slot = u16::from_le_bytes([bytes[0], bytes[1]]);
                    }
                }
                CpuTile::new(
                    w,
                    h,
                    1,
                    ColorSpace::Grayscale,
                    CpuTileLayout::Interleaved,
                    CpuTileData::u16(samples),
                )
            }
            other => Err(WsiError::Unsupported {
                reason: format!("unsupported ZVI raw sample byte depth {other}"),
            }),
        }
    }

    fn read_raw_rows(
        &self,
        plane: &ZviPlane,
        window: RawReadWindow,
        destination: &mut [u8],
    ) -> Result<(), WsiError> {
        let mut compound = self.compound.lock().unwrap_or_else(|e| e.into_inner());
        let mut stream = compound.open_stream(&plane.stream_path)?;
        let row_bytes = window.width as usize * window.bytes_per_sample as usize;
        for row in 0..window.height {
            let src_offset = plane
                .payload_offset
                .checked_add(
                    (u64::from(window.y + row) * u64::from(plane.width) + u64::from(window.x))
                        * window.bytes_per_sample,
                )
                .ok_or_else(|| WsiError::DisplayConversion("ZVI raw row offset overflow".into()))?;
            let dst = row as usize * row_bytes;
            stream.seek(SeekFrom::Start(src_offset))?;
            stream.read_exact(&mut destination[dst..dst + row_bytes])?;
        }
        Ok(())
    }

    fn read_zlib_plane_window(
        &self,
        plane: &ZviPlane,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<CpuTile, WsiError> {
        let compressed = self.read_plane_payload_to_end(plane)?;
        let mut decoder = ZlibDecoder::new(compressed.as_slice());
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        crop_decoded_zvi_plane(plane, &decompressed, x, y, w, h)
    }

    fn read_jpeg_plane_window(
        &self,
        plane: &ZviPlane,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<CpuTile, WsiError> {
        let jpeg = self.read_plane_payload_to_end(plane)?;
        let decoded = decode_batch_jpeg(&[JpegDecodeJob {
            data: std::borrow::Cow::Borrowed(jpeg.as_slice()),
            tables: None,
            expected_width: plane.width,
            expected_height: plane.height,
            color_transform: signinum_jpeg::ColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        }])
        .into_iter()
        .next()
        .ok_or_else(|| WsiError::Jpeg("empty ZVI JPEG decode result".into()))??;
        crop_interleaved_tile(&decoded, x, y, w, h)
    }

    fn read_plane_payload_to_end(&self, plane: &ZviPlane) -> Result<Vec<u8>, WsiError> {
        let mut compound = self.compound.lock().unwrap_or_else(|e| e.into_inner());
        let mut stream = compound.open_stream(&plane.stream_path)?;
        stream.seek(SeekFrom::Start(plane.payload_offset))?;
        let mut payload = Vec::new();
        stream.read_to_end(&mut payload)?;
        Ok(payload)
    }
}

fn crop_decoded_zvi_plane(
    plane: &ZviPlane,
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<CpuTile, WsiError> {
    match plane.bytes_per_sample {
        1 => {
            let mut samples = vec![0u8; w as usize * h as usize];
            for row in 0..h as usize {
                let src = ((y as usize + row) * plane.width as usize + x as usize)
                    .checked_mul(plane.bytes_per_sample as usize)
                    .ok_or_else(|| {
                        WsiError::DisplayConversion("ZVI decoded offset overflow".into())
                    })?;
                let dst = row * w as usize;
                samples[dst..dst + w as usize].copy_from_slice(&data[src..src + w as usize]);
            }
            CpuTile::new(
                w,
                h,
                1,
                ColorSpace::Grayscale,
                CpuTileLayout::Interleaved,
                CpuTileData::u8(samples),
            )
        }
        2 => {
            let mut samples = vec![0u16; w as usize * h as usize];
            for row in 0..h as usize {
                let src = ((y as usize + row) * plane.width as usize + x as usize)
                    .checked_mul(2)
                    .ok_or_else(|| {
                        WsiError::DisplayConversion("ZVI decoded offset overflow".into())
                    })?;
                let dst = row * w as usize;
                for (slot, bytes) in samples[dst..dst + w as usize]
                    .iter_mut()
                    .zip(data[src..src + w as usize * 2].chunks_exact(2))
                {
                    *slot = u16::from_le_bytes([bytes[0], bytes[1]]);
                }
            }
            CpuTile::new(
                w,
                h,
                1,
                ColorSpace::Grayscale,
                CpuTileLayout::Interleaved,
                CpuTileData::u16(samples),
            )
        }
        other => Err(WsiError::Unsupported {
            reason: format!("unsupported ZVI decoded sample byte depth {other}"),
        }),
    }
}

fn crop_interleaved_tile(
    src: &CpuTile,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<CpuTile, WsiError> {
    if src.layout != CpuTileLayout::Interleaved {
        return Err(WsiError::DisplayConversion(
            "cannot crop planar ZVI JPEG tile".into(),
        ));
    }
    let channels = src.channels as usize;
    let source = src
        .data
        .as_u8()
        .ok_or_else(|| WsiError::DisplayConversion("ZVI JPEG decoded to non-u8 samples".into()))?;
    let mut out = vec![0u8; width as usize * height as usize * channels];
    for row in 0..height as usize {
        let src_offset = ((y as usize + row) * src.width as usize + x as usize) * channels;
        let dst_offset = row * width as usize * channels;
        let len = width as usize * channels;
        out[dst_offset..dst_offset + len].copy_from_slice(&source[src_offset..src_offset + len]);
    }
    CpuTile::new(
        width,
        height,
        src.channels,
        src.color_space.clone(),
        CpuTileLayout::Interleaved,
        CpuTileData::u8(out),
    )
}
