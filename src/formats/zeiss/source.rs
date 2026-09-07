//! Source-block I/O and bounded reuse across neighboring CZI output tiles.
use super::preflight::preflight_czi_open_subblock_bounds;
use super::*;

impl ZeissSlide {
    pub(super) fn preflight_source_subblock(&self, offset: u64) -> Result<(), WsiError> {
        self.source_subblock_encoded_upper_bound(offset).map(|_| ())
    }

    pub(super) fn source_subblock_encoded_upper_bound(&self, offset: u64) -> Result<u64, WsiError> {
        if FileIdentity::from_path(&self.source_path)? != self.source_identity {
            return Err(WsiError::InvalidSlide {
                path: self.source_path.clone(),
                message: "CZI source identity check failed because the source path was replaced"
                    .into(),
            });
        }
        let mut file = self
            .preflight_file
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (actual_identity, bytes) =
            preflight_czi_open_subblock_bounds(&self.source_path, &mut file, offset, self.limits)?;
        if actual_identity != self.source_identity {
            return Err(WsiError::InvalidSlide {
                path: self.source_path.clone(),
                message: "CZI source identity check failed for the open preflight file".into(),
            });
        }
        Ok(bytes)
    }

    pub(super) fn read_source_subblock(
        &self,
        info: &czi_rs::DirectorySubBlockInfo,
    ) -> Result<czi_rs::RawSubBlock, WsiError> {
        self.preflight_source_subblock(info.file_position)?;
        self.czi
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .read_subblock(info.index)
            .map_err(|e| WsiError::DisplayConversion(e.to_string()))
    }

    pub(super) fn decoded_subblock(
        &self,
        info: &czi_rs::DirectorySubBlockInfo,
    ) -> Result<Arc<CpuTile>, WsiError> {
        self.resolve_subblock_claim(info, self.claim_subblock(info))
    }

    pub(super) fn claim_subblock(
        &self,
        info: &czi_rs::DirectorySubBlockInfo,
    ) -> crate::core::cache::TileClaim<'_, u64> {
        let cached = || {
            self.subblock_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&info.file_position)
                .cloned()
        };
        if let Some(tile) = cached() {
            return crate::core::cache::TileClaim::Ready(tile);
        }
        #[cfg(test)]
        if let Some(barrier) = &self.source_miss_barrier {
            barrier.wait();
        }
        self.source_flights.claim_miss(&info.file_position, cached)
    }

    pub(super) fn resolve_subblock_claim(
        &self,
        info: &czi_rs::DirectorySubBlockInfo,
        claim: crate::core::cache::TileClaim<'_, u64>,
    ) -> Result<Arc<CpuTile>, WsiError> {
        use crate::core::cache::TileClaim;
        let (cached, producer) = match claim {
            TileClaim::Ready(tile) => (Some(tile), None),
            TileClaim::Waiter(flight) => (flight.wait(), None),
            TileClaim::Producer(producer) => (None, Some(producer)),
            TileClaim::Uncoalesced => (None, None),
        };
        if let Some(tile) = cached {
            if FileIdentity::from_path(&self.source_path)? != self.source_identity {
                return Err(WsiError::InvalidSlide {
                    path: self.source_path.clone(),
                    message: "CZI source identity changed before subblock read".into(),
                });
            }
            return Ok(tile);
        }
        let raw = self.read_source_subblock(info)?;
        // Neither the seek lock nor the cache lock is held during decoding.
        #[cfg(test)]
        self.subblock_decodes.fetch_add(1, Ordering::Relaxed);
        let tile = Arc::new(super::subblock::tile_from_raw_subblock(&raw, self.limits)?);
        self.subblock_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(
                info.file_position,
                tile.clone(),
                tile.data.byte_size() as u64,
            );
        if let Some(producer) = producer {
            producer.complete(tile.clone());
        }
        Ok(tile)
    }
}
