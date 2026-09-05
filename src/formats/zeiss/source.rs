//! Source-block I/O and bounded reuse across neighboring CZI output tiles.
use super::preflight::preflight_czi_open_subblock_with_limits;
use super::*;

impl ZeissSlide {
    pub(super) fn preflight_source_subblock(&self, offset: u64) -> Result<(), WsiError> {
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
        let actual_identity = preflight_czi_open_subblock_with_limits(
            &self.source_path,
            &mut file,
            offset,
            self.limits,
        )?;
        if actual_identity != self.source_identity {
            return Err(WsiError::InvalidSlide {
                path: self.source_path.clone(),
                message: "CZI source identity check failed for the open preflight file".into(),
            });
        }
        Ok(())
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
        let cached = self
            .subblock_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&info.file_position)
            .cloned();
        if let Some(tile) = cached {
            // Cached pixels belong to the opened source, just like fresh data.
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
        Ok(tile)
    }
}
