use super::storage::{dataset_from_metadata, hex_encode, read_svcache};
use super::*;

impl SvcacheBackend {
    pub fn new() -> Self {
        Self
    }
}

impl FormatProbe for SvcacheBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let mut file = File::open(path)?;
        let mut magic = [0_u8; 8];
        if file.read_exact(&mut magic).is_err() {
            return Ok(ProbeResult {
                detected: false,
                vendor: "svcache".into(),
                confidence: ProbeConfidence::Likely,
            });
        }
        Ok(ProbeResult {
            detected: &magic == MAGIC,
            vendor: "svcache".into(),
            confidence: ProbeConfidence::Definite,
        })
    }
}

impl DatasetReader for SvcacheBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let (file, payload_start, metadata) = read_svcache(path)?;
        let dataset = dataset_from_metadata(path, &metadata);
        let associated_index = metadata
            .associated
            .iter()
            .enumerate()
            .map(|(idx, assoc)| (assoc.name.clone(), idx))
            .collect();
        Ok(Box::new(SvcacheReader {
            file: Mutex::new(file),
            payload_start,
            metadata,
            dataset,
            associated_index,
        }))
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

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        if matches!(output, TileOutputPreference::RequireDevice { .. }) {
            return Err(WsiError::Unsupported {
                reason: ".svcache device output is not implemented".into(),
            });
        }
        reqs.iter()
            .map(|req| self.read_tile_cpu(req).map(TilePixels::Cpu))
            .collect()
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
        let mut encoded = vec![0_u8; tile.payload_len as usize];
        {
            let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
            file.seek(SeekFrom::Start(self.payload_start + tile.payload_offset))?;
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
