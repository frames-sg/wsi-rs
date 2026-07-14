use super::compound::{compound_stream_paths, item_contents_index, read_stream_to_end};
use super::header::read_zvi_header;
use super::model::{ZviCompression, ZviPlane, ZviSlide};
use super::mosaic::{apply_mosaic_positions, build_mosaic_grid, build_zvi_channels};
use super::tags::{read_tags_if_present, tag_color, tag_f64, tag_string, tag_u32};
use super::*;

const DEFAULT_TILE_PX: u32 = 256;

pub(super) struct ZviReader {
    pub(super) slide: Arc<ZviSlide>,
}

impl SlideReader for ZviReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.slide.read_tile(req)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.slide
            .associated
            .get(name)
            .cloned()
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))
    }
}

impl ZviSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        let mut compound = cfb::open(path).map_err(|source| invalid_slide(path, source))?;
        let stream_paths = compound_stream_paths(&compound);
        let global_tags = read_tags_if_present(&mut compound, "/Image/Tags/Contents")?;
        let global_width = tag_u32(&global_tags, 515);
        let global_height = tag_u32(&global_tags, 516);
        let mpp_x = tag_f64(&global_tags, 769);
        let mpp_y = tag_f64(&global_tags, 772);

        let mut item_streams = stream_paths
            .iter()
            .filter_map(|stream_path| {
                item_contents_index(stream_path).map(|idx| (idx, stream_path.clone()))
            })
            .collect::<Vec<_>>();
        item_streams.sort_by_key(|(idx, _)| *idx);
        if item_streams.is_empty() {
            return Err(invalid_slide(path, "ZVI has no image item streams"));
        }

        let mut planes = Vec::with_capacity(item_streams.len());
        for (item_index, stream_path) in item_streams {
            let header = read_zvi_header(&mut compound, &stream_path)?;
            let stream_length = compound.open_stream(&stream_path)?.seek(SeekFrom::End(0))?;
            let payload_length = validated_payload_length(path, stream_length, &header)?;
            let tag_path = format!("/Image/Item({item_index})/Tags/Contents");
            let tags = read_tags_if_present(&mut compound, &tag_path)?;
            let stage_position = tag_f64(&tags, 2073).zip(tag_f64(&tags, 2074));
            planes.push(ZviPlane {
                stream_path,
                width: header.width,
                height: header.height,
                bytes_per_sample: header.bytes_per_sample,
                payload_offset: header.payload_offset,
                payload_length,
                compression: header.compression,
                z: header.z,
                c: header.c,
                t: header.t,
                tile_index: header.tile_index,
                stage_position,
                pixel_offset: (0, 0),
                grid_key: None,
                channel_name: tag_string(&tags, 1284),
                channel_color: tag_color(&tags, 1282),
            });
        }

        if planes.is_empty() {
            return Err(invalid_slide(
                path,
                "ZVI image item streams were not readable",
            ));
        }

        let sample_type = if planes.iter().all(|plane| plane.bytes_per_sample == 2) {
            SampleType::Uint16
        } else if planes.iter().all(|plane| plane.bytes_per_sample == 1) {
            SampleType::Uint8
        } else {
            return Err(invalid_slide(
                path,
                "mixed ZVI sample byte depths are not supported",
            ));
        };
        let max_z = planes.iter().map(|plane| plane.z).max().unwrap_or(0);
        let max_c = planes.iter().map(|plane| plane.c).max().unwrap_or(0);
        let max_t = planes.iter().map(|plane| plane.t).max().unwrap_or(0);
        let size_z = max_z + 1;
        let size_c = max_c + 1;
        let size_t = max_t + 1;
        let plane_width = planes.iter().map(|plane| plane.width).max().unwrap_or(0);
        let plane_height = planes.iter().map(|plane| plane.height).max().unwrap_or(0);
        let mosaic = planes.iter().any(|plane| plane.tile_index != 0)
            || global_width.is_some_and(|width| width > u64::from(plane_width))
            || global_height.is_some_and(|height| height > u64::from(plane_height));

        let mut plane_by_whole = HashMap::new();
        let mut plane_by_tile = HashMap::new();
        let level_dimensions;
        let tile_layout;
        if mosaic {
            let mpp = mpp_x.zip(mpp_y).ok_or_else(|| {
                invalid_slide(path, "ZVI mosaic is missing global pixel scaling tags")
            })?;
            apply_mosaic_positions(&mut planes, mpp);
            let grid = build_mosaic_grid(&mut planes, plane_width, plane_height);
            for (idx, plane) in planes.iter().enumerate() {
                if let Some((col, row)) = plane.grid_key {
                    plane_by_tile.insert((plane.z, plane.c, plane.t, col, row), idx);
                }
            }
            level_dimensions = (
                global_width.unwrap_or_else(|| grid.width.max(plane_width as u64)),
                global_height.unwrap_or_else(|| grid.height.max(plane_height as u64)),
            );
            tile_layout = TileLayout::Irregular {
                tile_advance: (grid.advance_x, grid.advance_y),
                extra_tiles: (2, 2, 2, 2),
                tiles: grid.entries,
            };
        } else {
            for (idx, plane) in planes.iter().enumerate() {
                plane_by_whole.insert((plane.z, plane.c, plane.t), idx);
            }
            level_dimensions = (
                global_width.unwrap_or(plane_width as u64),
                global_height.unwrap_or(plane_height as u64),
            );
            tile_layout = TileLayout::WholeLevel {
                width: level_dimensions.0,
                height: level_dimensions.1,
                virtual_tile_width: DEFAULT_TILE_PX,
                virtual_tile_height: DEFAULT_TILE_PX,
            };
        }

        let quickhash = quickhash_for_zvi(path, &planes, level_dimensions)?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash)?;
        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "zeiss");
        properties.insert("openslide.quickhash-1", quickhash);
        properties.insert("zeiss.format", "zvi");
        properties.insert("zeiss.image.size_x", level_dimensions.0.to_string());
        properties.insert("zeiss.image.size_y", level_dimensions.1.to_string());
        properties.insert("zeiss.image.size_z", size_z.to_string());
        properties.insert("zeiss.image.size_c", size_c.to_string());
        properties.insert("zeiss.image.size_t", size_t.to_string());
        if let Some(mpp_x) = mpp_x {
            properties.insert("openslide.mpp-x", format!("{mpp_x:.6}"));
        }
        if let Some(mpp_y) = mpp_y {
            properties.insert("openslide.mpp-y", format!("{mpp_y:.6}"));
        }
        if let Some(objective) = tag_string(&global_tags, 2049) {
            properties.insert("zeiss.objective.name", objective);
        }
        if let Some(power) = tag_string(&global_tags, 2076) {
            properties.insert("openslide.objective-power", power);
        }

        let channels = build_zvi_channels(&planes, size_c);
        let associated = associated_images(&mut compound)?;
        let associated_metadata = associated
            .iter()
            .map(|(name, tile)| {
                (
                    name.clone(),
                    AssociatedImage {
                        dimensions: (tile.width, tile.height),
                        sample_type: tile.data.sample_type(),
                        channels: tile.channels,
                    },
                )
            })
            .collect();

        let dataset = Dataset {
            id: dataset_id,
            scenes: vec![Scene {
                id: "scene_0".to_string(),
                name: Some("Image".to_string()),
                series: vec![Series {
                    id: "series_0".to_string(),
                    axes: AxesShape {
                        z: size_z,
                        c: size_c,
                        t: size_t,
                    },
                    levels: vec![Level {
                        dimensions: level_dimensions,
                        downsample: 1.0,
                        tile_layout,
                    }],
                    sample_type,
                    channels,
                }],
            }],
            associated_images: associated_metadata,
            properties,
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        };

        Ok(Self {
            dataset,
            compound: Mutex::new(compound),
            planes,
            plane_by_whole,
            plane_by_tile,
            associated,
        })
    }

    fn read_tile(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        if req.scene.get() != 0 || req.series.get() != 0 || req.level.get() != 0 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "ZVI exposes one scene, series, and level".into(),
            });
        }
        if req.plane.get().z >= self.dataset.scenes[0].series[0].axes.z
            || req.plane.get().c >= self.dataset.scenes[0].series[0].axes.c
            || req.plane.get().t >= self.dataset.scenes[0].series[0].axes.t
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "ZVI plane out of range".into(),
            });
        }

        let level = &self.dataset.scenes[0].series[0].levels[0];
        match &level.tile_layout {
            TileLayout::WholeLevel {
                width,
                height,
                virtual_tile_width,
                virtual_tile_height,
            } => {
                let plane_index = self
                    .plane_by_whole
                    .get(&(req.plane.get().z, req.plane.get().c, req.plane.get().t))
                    .copied()
                    .ok_or_else(|| WsiError::TileRead {
                        col: req.col,
                        row: req.row,
                        level: req.level.get(),
                        reason: "ZVI plane has no image payload".into(),
                    })?;
                let x = req.col.saturating_mul(i64::from(*virtual_tile_width));
                let y = req.row.saturating_mul(i64::from(*virtual_tile_height));
                if x < 0 || y < 0 || x >= *width as i64 || y >= *height as i64 {
                    return Err(WsiError::TileRead {
                        col: req.col,
                        row: req.row,
                        level: req.level.get(),
                        reason: "ZVI tile out of bounds".into(),
                    });
                }
                let w = (*virtual_tile_width).min((*width as i64 - x) as u32);
                let h = (*virtual_tile_height).min((*height as i64 - y) as u32);
                self.read_plane_window(plane_index, x as u32, y as u32, w, h)
            }
            TileLayout::Irregular { .. } => {
                let plane_index = self
                    .plane_by_tile
                    .get(&(
                        req.plane.get().z,
                        req.plane.get().c,
                        req.plane.get().t,
                        req.col,
                        req.row,
                    ))
                    .copied()
                    .ok_or_else(|| WsiError::TileRead {
                        col: req.col,
                        row: req.row,
                        level: req.level.get(),
                        reason: "ZVI mosaic tile not found".into(),
                    })?;
                let plane = &self.planes[plane_index];
                self.read_plane_window(plane_index, 0, 0, plane.width, plane.height)
            }
            TileLayout::Regular { .. } => Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "ZVI does not use regular native tiles".into(),
            }),
        }
    }
}

fn associated_images(
    compound: &mut CompoundFile<File>,
) -> Result<HashMap<String, CpuTile>, WsiError> {
    if !compound.is_stream("/Thumbnail") {
        return Ok(HashMap::new());
    }
    let data = read_stream_to_end(compound, "/Thumbnail")?;
    let Some(bmp_start) = data.windows(2).position(|bytes| bytes == b"BM") else {
        return Ok(HashMap::new());
    };
    let image = image::load_from_memory_with_format(&data[bmp_start..], ImageFormat::Bmp)
        .map_err(|source| WsiError::DisplayConversion(source.to_string()))?
        .to_rgb8();
    let tile = CpuTile::from_u8_interleaved(
        image.width(),
        image.height(),
        3,
        ColorSpace::Rgb,
        image.into_raw(),
    )?;
    Ok(HashMap::from([("thumbnail".to_string(), tile)]))
}

fn quickhash_for_zvi(
    path: &Path,
    planes: &[ZviPlane],
    dimensions: (u64, u64),
) -> Result<String, WsiError> {
    let mut quickhash = Quickhash1::new();
    quickhash.hash_string("zeiss-zvi");
    quickhash.hash_string(&path.display().to_string());
    quickhash.update(&dimensions.0.to_le_bytes());
    quickhash.update(&dimensions.1.to_le_bytes());
    for plane in planes.iter().take(64) {
        quickhash.hash_string(&plane.stream_path);
        quickhash.update(&plane.width.to_le_bytes());
        quickhash.update(&plane.height.to_le_bytes());
        quickhash.update(&plane.payload_offset.to_le_bytes());
        quickhash.update(&plane.payload_length.to_le_bytes());
    }
    quickhash
        .finish()
        .ok_or_else(|| WsiError::DisplayConversion("failed to compute ZVI quickhash".into()))
}

fn dataset_id_from_quickhash(path: &Path, quickhash: &str) -> Result<DatasetId, WsiError> {
    if quickhash.len() < 32 {
        return Err(invalid_slide(path, "ZVI quickhash too short"));
    }
    let value = u128::from_str_radix(&quickhash[..32], 16)
        .map_err(|_| invalid_slide(path, "ZVI quickhash is not valid hex"))?;
    Ok(DatasetId::new(value))
}

fn validated_payload_length(
    path: &Path,
    stream_length: u64,
    header: &super::model::ZviImageHeader,
) -> Result<u64, WsiError> {
    let payload_length = stream_length
        .checked_sub(header.payload_offset)
        .ok_or_else(|| invalid_slide(path, "ZVI payload offset exceeds stream length"))?;
    if payload_length > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
        return Err(invalid_slide(
            path,
            "ZVI plane payload exceeds safety limit",
        ));
    }
    if header.compression == ZviCompression::Raw {
        let expected = u64::from(header.width)
            .checked_mul(u64::from(header.height))
            .and_then(|value| value.checked_mul(u64::from(header.bytes_per_sample)))
            .ok_or_else(|| invalid_slide(path, "ZVI raw payload length overflow"))?;
        if payload_length != expected {
            return Err(invalid_slide(
                path,
                format!("ZVI raw payload has {payload_length} bytes, expected {expected}"),
            ));
        }
    }
    Ok(payload_length)
}

fn invalid_slide(path: &Path, message: impl ToString) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod payload_tests {
    use super::super::model::ZviImageHeader;
    use super::*;

    fn header(compression: ZviCompression) -> ZviImageHeader {
        ZviImageHeader {
            width: 2,
            height: 3,
            bytes_per_sample: 1,
            payload_offset: 10,
            compression,
            z: 0,
            c: 0,
            t: 0,
            tile_index: 0,
        }
    }

    #[test]
    fn payload_bounds_reject_offset_trailing_raw_data_and_oversize() {
        let path = Path::new("plane.zvi");
        assert_eq!(
            validated_payload_length(path, 16, &header(ZviCompression::Raw)).unwrap(),
            6
        );
        assert!(validated_payload_length(path, 9, &header(ZviCompression::Jpeg)).is_err());
        assert!(validated_payload_length(path, 17, &header(ZviCompression::Raw)).is_err());
        assert!(validated_payload_length(
            path,
            11 + crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
            &header(ZviCompression::Zlib),
        )
        .is_err());
    }
}
