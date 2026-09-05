use super::storage::{dataset_from_metadata, hex_encode, read_svcache_with_budget};
use super::*;

impl FormatProbe for SvcacheBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let mut file = File::open(path)?;
        let mut magic = [0_u8; 8];
        if file.read_exact(&mut magic).is_err() {
            return Ok(ProbeResult::not_detected("svcache"));
        }
        if &magic == MAGIC {
            return Ok(ProbeResult::detected("svcache", ProbeConfidence::Definite));
        }
        // Preserve the existing externally observable negative confidence.
        Ok(ProbeResult {
            detected: false,
            vendor: "svcache".into(),
            confidence: ProbeConfidence::Definite,
        })
    }
}

impl ConfiguredFormatProbe for SvcacheBackend {}

impl DatasetReader for SvcacheBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let reader = self.open_with_config(path, BackendOpenConfig::deterministic())?;
        Ok(reader)
    }
}

impl ConfiguredDatasetReader for SvcacheBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        let budget = OpenBudget::new(config.limits);
        let (file, payload_start, metadata) = read_svcache_with_budget(path, &budget)?;
        let dataset = dataset_from_metadata(path, &metadata);
        let associated_index_bytes = u64::try_from(metadata.associated.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(
                u64::try_from(std::mem::size_of::<(String, usize)>()).unwrap_or(u64::MAX),
            );
        budget.retain_index(associated_index_bytes)?;
        let mut associated_index = HashMap::new();
        associated_index
            .try_reserve(metadata.associated.len())
            .map_err(|_| WsiError::ResourceLimit {
                resource: "svcache associated-image index",
                requested: associated_index_bytes,
                limit: config.limits.tile_index_bytes(),
            })?;
        for (idx, assoc) in metadata.associated.iter().enumerate() {
            associated_index.insert(assoc.name.clone(), idx);
        }
        let reader = SvcacheReader {
            file: Mutex::new(file),
            payload_start,
            metadata,
            dataset,
            associated_index,
            encoded_unit_bytes: config.limits.encoded_unit_bytes(),
        };
        Ok(Box::new(reader))
    }
}

impl SlideReader for SvcacheReader {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let tile = self.tile_meta(req)?;
        self.read_tile_meta(tile)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let idx = self
            .associated_index
            .get(name)
            .copied()
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        self.read_tile_meta(&self.metadata.associated[idx].tile)
    }
}

impl ManagedSlideReader for SvcacheReader {
    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        Ok(self.tile_meta(req)?.payload_len)
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        reqs.iter().try_fold(0_u64, |total, req| {
            total
                .checked_add(self.tile_meta(req)?.payload_len)
                .ok_or(WsiError::ResourceLimit {
                    resource: "per-operation transient work",
                    requested: u64::MAX,
                    limit: self.encoded_unit_bytes,
                })
        })
    }

    fn display_tile_encoded_upper_bound(&self, _req: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn associated_encoded_upper_bound(&self, name: &str) -> Result<u64, WsiError> {
        let idx = self
            .associated_index
            .get(name)
            .copied()
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        Ok(self.metadata.associated[idx].tile.payload_len)
    }

    fn region_fastpath_encoded_upper_bound(&self, _req: &RegionRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }
}

impl SvcacheReader {
    fn tile_meta(&self, req: &TileRequest) -> Result<&TileMeta, WsiError> {
        let level = self
            .metadata
            .scenes
            .get(req.scene.get())
            .and_then(|scene| scene.series.get(req.series.get()))
            .and_then(|series| series.levels.get(req.level.get() as usize))
            .ok_or_else(|| WsiError::LevelOutOfRange {
                level: req.level.get(),
                count: 0,
            })?;
        if req.col < 0 || req.row < 0 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "negative .svcache tile coordinate".into(),
            });
        }
        let col = req.col as u64;
        let row = req.row as u64;
        if col >= level.tiles_across || row >= level.tiles_down {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: ".svcache tile coordinate out of range".into(),
            });
        }
        let idx = row
            .checked_mul(level.tiles_across)
            .and_then(|base| base.checked_add(col))
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: ".svcache tile index overflow".into(),
            })?;
        level
            .tile_meta_for_index(idx)
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: ".svcache tile not populated".into(),
            })
    }

    fn read_tile_meta(&self, tile: &TileMeta) -> Result<CpuTile, WsiError> {
        let payload_len =
            usize::try_from(tile.payload_len).map_err(|_| WsiError::InvalidSlide {
                path: PathBuf::from(&self.metadata.source.path),
                message: "svcache tile payload length is not addressable".into(),
            })?;
        let mut encoded = vec![0_u8; payload_len];
        {
            let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
            let offset = self
                .payload_start
                .checked_add(tile.payload_offset)
                .ok_or_else(|| WsiError::InvalidSlide {
                    path: PathBuf::from(&self.metadata.source.path),
                    message: "svcache tile payload offset overflow".into(),
                })?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut encoded)?;
        }
        let actual_hash = hex_encode(&Sha256::digest(&encoded));
        if actual_hash != tile.sha256 {
            return Err(WsiError::InvalidSlide {
                path: PathBuf::from(&self.metadata.source.path),
                message: "svcache tile checksum mismatch".into(),
            });
        }
        let decoded = match tile.codec {
            PayloadCodec::Zstd => {
                zstd::bulk::decompress(&encoded, tile.decoded_len).map_err(|err| {
                    WsiError::Codec {
                        codec: "svcache-zstd",
                        source: Box::new(err),
                    }
                })?
            }
        };
        CpuTile::from_u8_interleaved(
            tile.width,
            tile.height,
            tile.channels,
            tile.color_space.into(),
            decoded,
        )
    }
}
