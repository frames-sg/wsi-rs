use super::helpers::*;
use super::*;

mod parse;

impl MiraxSlide {
    #[cfg(test)]
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        Self::parse_with_cache_config(path, CacheConfig::deterministic())
    }

    pub(super) fn decode_image_with_backend(
        &self,
        image: &Arc<MiraxImage>,
        _backend: BackendRequest,
    ) -> Result<Arc<CpuTile>, WsiError> {
        self.resolve_image_claim(image, self.claim_image(image))
    }

    pub(super) fn claim_image(&self, image: &MiraxImage) -> crate::core::cache::TileClaim<'_, u32> {
        let cached = || {
            self.decoded_images
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&image.id)
                .cloned()
        };
        if let Some(tile) = cached() {
            return crate::core::cache::TileClaim::Ready(tile);
        }
        #[cfg(test)]
        if let Some(barrier) = &self.source_miss_barrier {
            barrier.wait();
        }
        self.source_flights.claim_miss(&image.id, cached)
    }

    pub(super) fn resolve_image_claim(
        &self,
        image: &MiraxImage,
        claim: crate::core::cache::TileClaim<'_, u32>,
    ) -> Result<Arc<CpuTile>, WsiError> {
        use crate::core::cache::TileClaim;
        let producer = match claim {
            TileClaim::Ready(tile) => return Ok(tile),
            TileClaim::Waiter(flight) => {
                if let Some(tile) = flight.wait() {
                    return Ok(tile);
                }
                None
            }
            TileClaim::Producer(producer) => Some(producer),
            TileClaim::Uncoalesced => None,
        };
        #[cfg(test)]
        self.source_decodes.fetch_add(1, Ordering::Relaxed);
        let decoded = Arc::new(self.decode_record_to_sample_buffer(
            &image.record,
            image.format,
            Some((image.expected_width, image.expected_height)),
            BackendRequest::Auto,
        )?);
        let retained_bytes = u64::try_from(decoded.data.byte_size()).unwrap_or(u64::MAX);
        self.decoded_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(image.id, decoded.clone(), retained_bytes);
        if let Some(producer) = producer {
            producer.complete(decoded.clone());
        }
        Ok(decoded)
    }

    pub(super) fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let record = self
            .associated
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        if let Some(buffer) = self
            .associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            #[cfg(test)]
            {
                MIRAX_ASSOCIATED_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            }
            return Ok((*buffer).clone());
        }
        let decoded = Arc::new(self.decode_record_to_sample_buffer(
            record,
            MiraxImageFormat::Jpeg,
            None,
            BackendRequest::Auto,
        )?);
        let retained_bytes = u64::try_from(decoded.data.byte_size()).unwrap_or(u64::MAX);
        self.associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(name.to_string(), decoded.clone(), retained_bytes);
        Ok((*decoded).clone())
    }

    fn decode_record_to_sample_buffer(
        &self,
        record: &MiraxRecord,
        format: MiraxImageFormat,
        expected_dimensions: Option<(u32, u32)>,
        _backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let bytes = self.read_record_bytes(record)?;
        match format {
            MiraxImageFormat::Jpeg => {
                let (expected_width, expected_height) = expected_dimensions.unwrap_or((0, 0));
                crate::core::batch::exactly_one(
                    decode_batch_jpeg(&[JpegDecodeJob {
                        data: Cow::Borrowed(&bytes),
                        tables: None,
                        expected_width,
                        expected_height,
                        color_transform: j2k_jpeg::ColorTransform::Auto,
                        force_dimensions: false,
                        requested_size: None,
                    }]),
                    "MIRAX JPEG decode",
                )?
            }
            MiraxImageFormat::Png | MiraxImageFormat::Bmp24 => {
                let image = image::load_from_memory(&bytes)
                    .map_err(|err| {
                        WsiError::DisplayConversion(format!("failed to decode MIRAX image: {err}"))
                    })?
                    .to_rgb8();
                if let Some((expected_width, expected_height)) = expected_dimensions {
                    if image.width() != expected_width || image.height() != expected_height {
                        return Err(WsiError::DisplayConversion(format!(
                            "MIRAX image dimensions mismatch: expected {}x{}, got {}x{}",
                            expected_width,
                            expected_height,
                            image.width(),
                            image.height()
                        )));
                    }
                }
                Ok(rgb_image_to_sample_buffer(image))
            }
        }
    }

    pub(super) fn read_record_bytes(&self, record: &MiraxRecord) -> Result<Vec<u8>, WsiError> {
        let file = self.open_file_handle(&record.path)?;
        let len = crate::core::limits::checked_product_to_usize(
            &[record.len],
            self.encoded_unit_bytes,
            "MIRAX record",
        )
        .map_err(|message| invalid_slide(&record.path, message))?;
        let mut bytes = vec![0; len];
        file.read_exact_at(&mut bytes, record.offset)
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: record.path.clone(),
            })?;
        Ok(bytes)
    }

    fn open_file_handle(
        &self,
        path: &Path,
    ) -> Result<Arc<crate::core::positioned_file::PositionedFile>, WsiError> {
        if let Some(file) = self
            .open_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
        {
            return Ok(Arc::clone(file));
        }

        let file = File::open(path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
        let file = Arc::new(crate::core::positioned_file::PositionedFile::new(file));
        Ok(Arc::clone(
            self.open_files
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(path.to_path_buf())
                .or_insert(file),
        ))
    }
}
