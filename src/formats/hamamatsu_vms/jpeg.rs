use super::model::{invalid_slide, VmsJpeg};
use super::*;

const JPEG_HEADER_MAX_BYTES: usize = 1 << 20;
const JPEG_SCAN_CHUNK_BYTES: usize = 64 << 10;
const VMS_DECODED_TILE_CACHE_ENTRIES: usize = 64;

impl VmsJpeg {
    pub(super) fn parse(path: &Path, row_starts: Vec<Option<u64>>) -> Result<Self, WsiError> {
        let header = read_vms_jpeg_header(path).map_err(|err| {
            invalid_slide(
                path,
                format!("failed to derive VMS JPEG tile geometry: {err}"),
            )
        })?;
        let geometry = header.geometry;
        let tiles_across = geometry.width.div_ceil(geometry.tile_width);
        let tiles_down = geometry.height.div_ceil(geometry.tile_height);
        let tile_count = tiles_across as usize * tiles_down as usize;
        let mut unreliable_mcu_starts = vec![None; tile_count];
        for (row, offset) in row_starts.into_iter().enumerate().take(tiles_down as usize) {
            let idx = row * tiles_across as usize;
            if idx < unreliable_mcu_starts.len() {
                unreliable_mcu_starts[idx] = offset;
            }
        }
        let mut mcu_starts = vec![None; tile_count];
        if !mcu_starts.is_empty() {
            mcu_starts[0] = Some(header.scan_data_offset);
        }
        let file = File::open(path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;

        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            header: header.header,
            sof_dimensions_offset: header.sof_dimensions_offset,
            file_len: header.file_len,
            width: geometry.width,
            height: geometry.height,
            tile_width: geometry.tile_width,
            tile_height: geometry.tile_height,
            tiles_across,
            tiles_down,
            mcu_starts: Mutex::new(mcu_starts),
            unreliable_mcu_starts,
            decoded_tile_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(VMS_DECODED_TILE_CACHE_ENTRIES).unwrap(),
            )),
            comment: header.comment,
        })
    }

    pub(super) fn decode_tile(
        &self,
        tile_index: usize,
        scale_denom: u32,
        _backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let cache_key = (tile_index, scale_denom);
        if let Some(cached) = self
            .decoded_tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cache_key)
        {
            return Ok(cached.as_ref().clone());
        }

        let scale = match scale_denom {
            1 => J2kDownscale::None,
            2 => J2kDownscale::Half,
            4 => J2kDownscale::Quarter,
            8 => J2kDownscale::Eighth,
            other => {
                return Err(WsiError::Jpeg(format!(
                    "unsupported VMS j2k downscale denominator {other}"
                )));
            }
        };
        let tile_col = tile_index as u32 % self.tiles_across;
        let tile_row = tile_index as u32 / self.tiles_across;
        let width = self
            .tile_width
            .min(self.width.saturating_sub(tile_col * self.tile_width));
        let height = self
            .tile_height
            .min(self.height.saturating_sub(tile_row * self.tile_height));
        let data = self.tile_jpeg_bytes(tile_index, width, height)?;
        let decoder = J2kJpegDecoder::new(&data).map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let roi = J2kRect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        let (pixels, _outcome) = decoder
            .decode_region_scaled(J2kPixelFormat::Rgb8, roi, scale)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let scale_denom = scale.denominator();
        let width = width.div_ceil(scale_denom);
        let height = height.div_ceil(scale_denom);
        let tile = CpuTile {
            width,
            height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(pixels),
        };
        self.decoded_tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(cache_key, Arc::new(tile.clone()));
        Ok(tile)
    }

    fn tile_jpeg_bytes(
        &self,
        tile_index: usize,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, WsiError> {
        let (start, stop) = self.tile_entropy_bounds(tile_index)?;
        if stop <= start || stop > self.file_len {
            return Err(WsiError::Jpeg(format!(
                "invalid VMS JPEG entropy bounds {}..{} for {}",
                start,
                stop,
                self.path.display()
            )));
        }
        let data_len = usize::try_from(stop - start)
            .map_err(|_| WsiError::Jpeg("VMS JPEG entropy segment is too large".into()))?;
        let mut entropy = vec![0u8; data_len];
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(start))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.path.clone(),
            })?;
        file.read_exact(&mut entropy)
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.path.clone(),
            })?;
        if entropy.len() < 2 || entropy[entropy.len() - 2] != 0xFF {
            return Err(WsiError::Jpeg(format!(
                "VMS JPEG entropy segment for {} does not end at a marker",
                self.path.display()
            )));
        }
        let last = entropy.len() - 1;
        entropy[last] = 0xD9;

        let mut header = self.header.clone();
        patch_sof_dimensions(&mut header, self.sof_dimensions_offset, width, height)?;
        header.extend_from_slice(&entropy);
        Ok(header)
    }

    fn tile_entropy_bounds(&self, tile_index: usize) -> Result<(u64, u64), WsiError> {
        let tile_count = self.tiles_across as usize * self.tiles_down as usize;
        if tile_index >= tile_count {
            return Err(WsiError::Jpeg(format!(
                "VMS JPEG tile index {tile_index} out of range {tile_count}"
            )));
        }
        let mut starts = self.mcu_starts.lock().unwrap_or_else(|e| e.into_inner());
        self.ensure_mcu_start(&mut starts, tile_index)?;
        let start = starts[tile_index].ok_or_else(|| {
            WsiError::Jpeg(format!("missing VMS JPEG MCU start for tile {tile_index}"))
        })?;
        let stop = if tile_index + 1 == tile_count {
            self.file_len
        } else {
            self.ensure_mcu_start(&mut starts, tile_index + 1)?;
            starts[tile_index + 1].ok_or_else(|| {
                WsiError::Jpeg(format!(
                    "missing VMS JPEG MCU stop for tile {}",
                    tile_index + 1
                ))
            })?
        };
        Ok((start, stop))
    }

    fn ensure_mcu_start(&self, starts: &mut [Option<u64>], target: usize) -> Result<(), WsiError> {
        if target >= starts.len() || starts[target].is_some() {
            return Ok(());
        }
        if starts[0].is_none() {
            starts[0] = Some(self.header.len() as u64);
        }

        let mut first_good = target;
        loop {
            if starts[first_good].is_some() {
                break;
            }
            if let Some(offset) = self
                .unreliable_mcu_starts
                .get(first_good)
                .and_then(|offset| *offset)
            {
                if first_good == 0 || self.valid_recorded_restart_offset(offset)? {
                    starts[first_good] = Some(offset);
                    break;
                }
            }
            if first_good == 0 {
                starts[0] = Some(self.header.len() as u64);
                break;
            }
            first_good -= 1;
        }

        let mut offset = starts[first_good].ok_or_else(|| {
            WsiError::Jpeg(format!(
                "missing VMS JPEG known MCU start before tile {target}"
            ))
        })?;
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.path.clone(),
            })?;
        for idx in first_good..target {
            offset = find_next_restart_offset(&mut file, self.file_len, &self.path)?.ok_or_else(
                || {
                    WsiError::Jpeg(format!(
                        "could not find restart marker for VMS JPEG tile {} in {}",
                        idx + 1,
                        self.path.display()
                    ))
                },
            )?;
            starts[idx + 1] = Some(offset);
            file.seek(SeekFrom::Start(offset))
                .map_err(|source| WsiError::IoWithPath {
                    source: Arc::new(source),
                    path: self.path.clone(),
                })?;
        }
        Ok(())
    }

    fn valid_recorded_restart_offset(&self, offset: u64) -> Result<bool, WsiError> {
        if offset == self.header.len() as u64 {
            return Ok(true);
        }
        if offset < 2 || offset > self.file_len {
            return Ok(false);
        }
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(offset - 2))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.path.clone(),
            })?;
        let mut marker = [0u8; 2];
        file.read_exact(&mut marker)
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.path.clone(),
            })?;
        Ok(marker[0] == 0xFF && (0xD0..=0xD7).contains(&marker[1]))
    }
}

pub(super) struct VmsJpegHeader {
    pub(super) header: Vec<u8>,
    pub(super) geometry: JpegTileGeometry,
    pub(super) sof_dimensions_offset: usize,
    pub(super) scan_data_offset: u64,
    pub(super) file_len: u64,
    pub(super) comment: Option<String>,
}

pub(super) fn read_vms_jpeg_header(path: &Path) -> Result<VmsJpegHeader, WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let file_len = file
        .metadata()
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?
        .len();
    let mut header = Vec::with_capacity(4096);
    let mut soi = [0u8; 2];
    read_exact_header(&mut file, path, &mut header, &mut soi)?;
    if soi != [0xFF, 0xD8] {
        return Err(WsiError::Jpeg("VMS JPEG missing SOI marker".into()));
    }

    let mut dimensions = None;
    let mut sof_dimensions_offset = None;
    let mut max_h = 0u8;
    let mut max_v = 0u8;
    let mut restart_interval = None;
    let mut comment = None;

    loop {
        let marker_start = header.len();
        let marker = read_next_header_marker(&mut file, path, &mut header)?;
        match marker {
            0xD9 => return Err(WsiError::Jpeg("VMS JPEG ended before SOS".into())),
            0x01 | 0xD0..=0xD7 => continue,
            _ => {}
        }

        let mut len_bytes = [0u8; 2];
        read_exact_header(&mut file, path, &mut header, &mut len_bytes)?;
        let seg_len = u16::from_be_bytes(len_bytes) as usize;
        if seg_len < 2 {
            return Err(WsiError::Jpeg(format!(
                "invalid VMS JPEG segment length {seg_len}"
            )));
        }
        let payload_start = header.len();
        let mut payload = vec![0u8; seg_len - 2];
        read_exact_header(&mut file, path, &mut header, &mut payload)?;

        match marker {
            0xC0..=0xC2 => {
                if payload.len() < 6 {
                    return Err(WsiError::Jpeg("truncated VMS JPEG SOF segment".into()));
                }
                if payload[0] != 8 {
                    return Err(WsiError::Jpeg(format!(
                        "unsupported VMS JPEG precision {}",
                        payload[0]
                    )));
                }
                let height = u16::from_be_bytes([payload[1], payload[2]]) as u32;
                let width = u16::from_be_bytes([payload[3], payload[4]]) as u32;
                let components = payload[5] as usize;
                if components == 0 || payload.len() < 6 + components * 3 {
                    return Err(WsiError::Jpeg("truncated VMS JPEG component list".into()));
                }
                max_h = 0;
                max_v = 0;
                for idx in 0..components {
                    let sampling = payload[6 + idx * 3 + 1];
                    max_h = max_h.max(sampling >> 4);
                    max_v = max_v.max(sampling & 0x0F);
                }
                if width == 0 || height == 0 || max_h == 0 || max_v == 0 {
                    return Err(WsiError::Jpeg(
                        "invalid VMS JPEG dimensions or sampling".into(),
                    ));
                }
                dimensions = Some((width, height));
                sof_dimensions_offset = Some(payload_start + 1);
            }
            0xDD => {
                if payload.len() < 2 {
                    return Err(WsiError::Jpeg("truncated VMS JPEG DRI segment".into()));
                }
                let interval = u16::from_be_bytes([payload[0], payload[1]]);
                if interval != 0 {
                    restart_interval = Some(interval);
                }
            }
            0xFE if comment.is_none() => {
                let end = payload
                    .iter()
                    .position(|b| *b == 0)
                    .unwrap_or(payload.len());
                comment = Some(String::from_utf8_lossy(&payload[..end]).into_owned());
            }
            0xFE => {}
            0xDA => {
                let (width, height) = dimensions
                    .ok_or_else(|| WsiError::Jpeg("VMS JPEG missing SOF before SOS".into()))?;
                let restart_interval = restart_interval.ok_or_else(|| {
                    WsiError::Jpeg(
                        "VMS JPEG missing restart interval required for tile geometry".into(),
                    )
                })?;
                let mcu_width = u32::from(max_h) * 8;
                let mcu_height = u32::from(max_v) * 8;
                let mcus_per_row = width.div_ceil(mcu_width);
                let restart = u32::from(restart_interval);
                if restart > mcus_per_row {
                    return Err(WsiError::Jpeg(
                        "VMS JPEG restart interval greater than MCUs per row".into(),
                    ));
                }
                if mcus_per_row % restart != 0 {
                    return Err(WsiError::Jpeg(
                        "VMS JPEG restart interval does not align to MCU rows".into(),
                    ));
                }
                let tile_width = mcu_width
                    .checked_mul(restart)
                    .ok_or_else(|| WsiError::Jpeg("VMS JPEG tile width overflow".into()))?;
                let scan_data_offset = u64::try_from(header.len()).map_err(|_| {
                    WsiError::Jpeg("VMS JPEG header offset does not fit u64".into())
                })?;
                let sof_dimensions_offset = sof_dimensions_offset.ok_or_else(|| {
                    WsiError::Jpeg("VMS JPEG missing SOF dimensions offset".into())
                })?;
                if marker_start >= header.len() {
                    return Err(WsiError::Jpeg("invalid VMS JPEG marker accounting".into()));
                }
                return Ok(VmsJpegHeader {
                    header,
                    geometry: JpegTileGeometry {
                        width,
                        height,
                        tile_width,
                        tile_height: mcu_height,
                    },
                    sof_dimensions_offset,
                    scan_data_offset,
                    file_len,
                    comment,
                });
            }
            _ => {}
        }
    }
}

fn read_exact_header(
    file: &mut File,
    path: &Path,
    header: &mut Vec<u8>,
    buf: &mut [u8],
) -> Result<(), WsiError> {
    if header.len().saturating_add(buf.len()) > JPEG_HEADER_MAX_BYTES {
        return Err(WsiError::Jpeg(format!(
            "VMS JPEG header exceeds {} bytes: {}",
            JPEG_HEADER_MAX_BYTES,
            path.display()
        )));
    }
    file.read_exact(buf)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    header.extend_from_slice(buf);
    Ok(())
}

fn read_next_header_marker(
    file: &mut File,
    path: &Path,
    header: &mut Vec<u8>,
) -> Result<u8, WsiError> {
    let mut byte = [0u8; 1];
    loop {
        read_exact_header(file, path, header, &mut byte)?;
        if byte[0] == 0xFF {
            break;
        }
    }
    loop {
        read_exact_header(file, path, header, &mut byte)?;
        if byte[0] != 0xFF {
            return Ok(byte[0]);
        }
    }
}

fn patch_sof_dimensions(
    header: &mut [u8],
    dimensions_offset: usize,
    width: u32,
    height: u32,
) -> Result<(), WsiError> {
    let width = u16::try_from(width)
        .map_err(|_| WsiError::Jpeg(format!("VMS JPEG tile width {width} exceeds u16")))?;
    let height = u16::try_from(height)
        .map_err(|_| WsiError::Jpeg(format!("VMS JPEG tile height {height} exceeds u16")))?;
    if dimensions_offset + 4 > header.len() {
        return Err(WsiError::Jpeg(
            "VMS JPEG SOF dimensions offset is outside header".into(),
        ));
    }
    header[dimensions_offset..dimensions_offset + 2].copy_from_slice(&height.to_be_bytes());
    header[dimensions_offset + 2..dimensions_offset + 4].copy_from_slice(&width.to_be_bytes());
    Ok(())
}

fn find_next_restart_offset(
    file: &mut File,
    file_len: u64,
    path: &Path,
) -> Result<Option<u64>, WsiError> {
    let mut buf = [0u8; JPEG_SCAN_CHUNK_BYTES];
    let mut pending_ff = false;
    loop {
        let base = file
            .stream_position()
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        if base >= file_len {
            return Ok(None);
        }
        let n = file.read(&mut buf).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
        if n == 0 {
            return Ok(None);
        }
        for (idx, byte) in buf[..n].iter().copied().enumerate() {
            if pending_ff {
                if byte == 0xFF {
                    continue;
                }
                pending_ff = false;
                if byte == 0x00 {
                    continue;
                }
                if (0xD0..=0xD7).contains(&byte) {
                    return Ok(Some(base + idx as u64 + 1));
                }
                if byte == 0xD9 {
                    return Ok(None);
                }
                return Err(WsiError::Jpeg(format!(
                    "unexpected JPEG marker FF{byte:02X} while scanning {}",
                    path.display()
                )));
            } else if byte == 0xFF {
                pending_ff = true;
            }
        }
    }
}
