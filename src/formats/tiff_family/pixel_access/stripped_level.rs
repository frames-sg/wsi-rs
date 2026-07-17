use super::*;

impl TiffPixelReader {
    fn decode_stripped_level_image(
        &self,
        ifd_id: IfdId,
        compression: Compression,
        dimensions: (u32, u32),
        strip_offsets: &[u64],
        strip_byte_counts: &[u64],
    ) -> Result<CpuTile, WsiError> {
        if compression != Compression::None {
            return Err(WsiError::UnsupportedFormat(format!(
                "generic stripped TIFF levels require uncompressed RGB data, got {compression:?}"
            )));
        }
        let data =
            self.read_stripped_data("generic TIFF level", strip_offsets, strip_byte_counts)?;
        let planar = self
            .container
            .get_u32(ifd_id, tags::PLANAR_CONFIGURATION)
            .unwrap_or(1);
        if planar != 2 {
            return self.decode_uncompressed_tile(ifd_id, &data, dimensions.0, dimensions.1);
        }

        let samples_per_pixel = self
            .container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1);
        let bits_per_sample = self
            .container
            .get_u64_array(ifd_id, tags::BITS_PER_SAMPLE)
            .map_err(|err| err.into_wsi_error(self.container.path()))?;
        let photometric = self
            .container
            .get_u32(ifd_id, tags::PHOTOMETRIC)
            .unwrap_or(0);
        let sample_format = self
            .container
            .get_u64_array(ifd_id, tags::SAMPLE_FORMAT)
            .ok();
        if samples_per_pixel != 3
            || bits_per_sample.is_empty()
            || bits_per_sample.iter().any(|&bits| bits != 8)
            || photometric != 2
            || sample_format.is_some_and(|formats| formats.iter().any(|&format| format != 1))
        {
            return Err(WsiError::UnsupportedFormat(
                "generic planar stripped TIFF levels require unsigned 8-bit RGB samples".into(),
            ));
        }

        let plane_len = checked_product_to_usize(
            &[u64::from(dimensions.0), u64::from(dimensions.1)],
            MAX_DECODED_IMAGE_BYTES,
            "generic planar stripped TIFF sample plane",
        )
        .map_err(WsiError::DisplayConversion)?;
        let expected_len = plane_len.checked_mul(3).ok_or_else(|| {
            WsiError::DisplayConversion(
                "generic planar stripped TIFF RGB byte count overflow".into(),
            )
        })?;
        if data.len() != expected_len {
            return Err(WsiError::UnsupportedFormat(format!(
                "generic planar stripped TIFF has {} decoded bytes, expected {expected_len}",
                data.len()
            )));
        }

        let mut interleaved = vec![0u8; expected_len];
        for pixel in 0..plane_len {
            interleaved[pixel * 3] = data[pixel];
            interleaved[pixel * 3 + 1] = data[plane_len + pixel];
            interleaved[pixel * 3 + 2] = data[plane_len * 2 + pixel];
        }
        Ok(CpuTile {
            width: dimensions.0,
            height: dimensions.1,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(interleaved),
        })
    }

    fn get_or_decode_stripped_level_image(
        &self,
        ifd_id: IfdId,
        compression: Compression,
        dimensions: (u32, u32),
        strip_offsets: &[u64],
        strip_byte_counts: &[u64],
    ) -> Result<Arc<CpuTile>, WsiError> {
        if let Some(image) = self
            .full_decode_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(&ifd_id)
        {
            return Ok(image);
        }

        let image = Arc::new(self.decode_stripped_level_image(
            ifd_id,
            compression,
            dimensions,
            strip_offsets,
            strip_byte_counts,
        )?);
        self.full_decode_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .put(ifd_id, image.clone());
        Ok(image)
    }

    pub(super) fn read_stripped_level_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        compression: Compression,
        strip_offsets: &[u64],
        strip_byte_counts: &[u64],
    ) -> Result<CpuTile, WsiError> {
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = level.tile_layout
        else {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "generic stripped TIFF level expects a regular tile layout".into(),
            });
        };
        let (col, row) = validate_tile_coords(req.col, req.row, req.level.get())?;
        if u64::from(col) >= tiles_across || u64::from(row) >= tiles_down {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("tile is outside the {tiles_across}x{tiles_down} grid"),
            });
        }

        let dimensions = (
            u32::try_from(level.dimensions.0).map_err(|_| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "generic stripped TIFF width exceeds u32".into(),
            })?,
            u32::try_from(level.dimensions.1).map_err(|_| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "generic stripped TIFF height exceeds u32".into(),
            })?,
        );
        let src_x = col * tile_width;
        let src_y = row * tile_height;
        let width = tile_width.min(dimensions.0 - src_x);
        let height = tile_height.min(dimensions.1 - src_y);
        let image = self.get_or_decode_stripped_level_image(
            ifd_id,
            compression,
            dimensions,
            strip_offsets,
            strip_byte_counts,
        )?;
        crop_rgb_interleaved_u8_buffer(image.as_ref(), src_x, src_y, width, height)
    }
}
