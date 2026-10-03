use super::helpers::*;
use super::*;
use crate::core::execution_telemetry::{self, Event};
use crate::core::file_identity::FileIdentity;
use std::sync::Weak;

mod parse;

/// Indexes held by open MIRAX handles. Entries are few and `SlideLimits` is
/// not `Hash`, so lookup is linear.
static SHARED_INDEXES: Mutex<Vec<(MiraxShareKey, Weak<MiraxShared>)>> = Mutex::new(Vec::new());

/// The files a MIRAX index was parsed from and the limits it was validated
/// under. Keying on the complete limits means an index parsed under looser
/// limits never serves a stricter open.
#[derive(PartialEq, Eq)]
struct MiraxShareKey {
    limits: crate::SlideLimits,
    /// Data file paths as Slidedat.ini resolved them, which records retain.
    datafile_paths: Vec<PathBuf>,
    /// Slidedat.ini, the index and every data file.
    identities: Vec<FileIdentity>,
}

impl MiraxShareKey {
    /// `None` when a file cannot be identified. That open parses without
    /// sharing and reports any error the parse itself meets.
    fn new(
        slidedat: &Path,
        index: &Path,
        datafiles: &[PathBuf],
        limits: crate::SlideLimits,
    ) -> Option<Self> {
        let identities = [slidedat, index]
            .into_iter()
            .chain(datafiles.iter().map(PathBuf::as_path))
            .map(|path| FileIdentity::from_path(path).ok())
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            limits,
            datafile_paths: datafiles.to_vec(),
            identities,
        })
    }
}

fn shared_index(key: &MiraxShareKey) -> Option<Arc<MiraxShared>> {
    SHARED_INDEXES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .find(|(entry, _)| entry == key)
        .and_then(|(_, shared)| shared.upgrade())
}

/// Registers a freshly parsed index. When a concurrent open registered the
/// same key first, returns that one so only one copy stays alive.
fn register_shared_index(key: MiraxShareKey, shared: MiraxShared) -> Arc<MiraxShared> {
    let mut registry = SHARED_INDEXES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    registry.retain(|(_, shared)| shared.strong_count() > 0);
    if let Some(existing) = registry
        .iter()
        .find(|(entry, _)| *entry == key)
        .and_then(|(_, shared)| shared.upgrade())
    {
        return existing;
    }
    let shared = Arc::new(shared);
    registry.push((key, Arc::downgrade(&shared)));
    shared
}

impl MiraxSlide {
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
        self.probe.wait_before_miss();
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
        self.probe.record_decode();
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
            .shared
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
            execution_telemetry::record(Event::MiraxAssociatedCacheHits, 1);
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
                path: record.path.as_ref().clone(),
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
