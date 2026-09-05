use super::*;

impl TiffLayoutInterpreter for VentanaInterpreter {
    fn vendor_name(&self) -> &'static str {
        "ventana"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        for &ifd_id in container.top_ifds() {
            if has_iscan_xmp(container, ifd_id) {
                return true;
            }
        }
        false
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "ventana");

        // Phase 1: Find and parse the iScan element from XMP for vendor properties.
        let xmp_str = find_xmp_string(container)?;
        if let Some(ref xmp) = xmp_str {
            parse_iscan_properties(xmp, &mut properties);
        }

        // Phase 2: Classify public associated images and the stitched pyramid.
        let mut pyramid_ifds = Vec::new();
        let mut associated_images: HashMap<String, AssociatedImage> = HashMap::new();
        let mut associated_sources: HashMap<String, TileSource> = HashMap::new();

        for &ifd_id in container.top_ifds() {
            let width = match container.get_u64(ifd_id, tags::IMAGE_WIDTH) {
                Ok(v) if v > 0 => v,
                _ => continue,
            };
            let height = match container.get_u64(ifd_id, tags::IMAGE_LENGTH) {
                Ok(v) if v > 0 => v,
                _ => continue,
            };
            let desc = container
                .get_string(ifd_id, tags::IMAGE_DESCRIPTION)
                .unwrap_or("")
                .to_ascii_lowercase();

            if let Some(name) = classify_associated_image(&desc) {
                let compression =
                    compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1));
                let source = if let (Ok(tile_width), Ok(tile_height)) = (
                    container.get_u32(ifd_id, tags::TILE_WIDTH),
                    container.get_u32(ifd_id, tags::TILE_LENGTH),
                ) {
                    if tile_width == 0 || tile_height == 0 {
                        continue;
                    }
                    TileSource::TiledIfd {
                        ifd_id,
                        jpeg_tables: container
                            .get_bytes(ifd_id, tags::JPEG_TABLES)
                            .ok()
                            .map(|b| b.to_vec()),
                        compression,
                    }
                } else {
                    TileSource::Stripped {
                        ifd_id,
                        jpeg_tables: container
                            .get_bytes(ifd_id, tags::JPEG_TABLES)
                            .ok()
                            .map(|b| b.to_vec()),
                        compression,
                        strip_offsets: container
                            .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                            .map(|values| values.to_vec())
                            .unwrap_or_default(),
                        strip_byte_counts: container
                            .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                            .map(|values| values.to_vec())
                            .unwrap_or_default(),
                    }
                };

                associated_images.insert(
                    name.clone(),
                    AssociatedImage {
                        dimensions: (
                            u32::try_from(width).unwrap_or(u32::MAX),
                            u32::try_from(height).unwrap_or(u32::MAX),
                        ),
                        sample_type: SampleType::Uint8,
                        channels: 3,
                        icc_profile: Vec::new(),
                    },
                );
                associated_sources.insert(name, source);
                continue;
            }

            if !desc.contains("level=") {
                continue;
            }
            let tile_width = match container.get_u32(ifd_id, tags::TILE_WIDTH) {
                Ok(v) if v > 0 => v,
                _ => continue,
            };
            let tile_height = match container.get_u32(ifd_id, tags::TILE_LENGTH) {
                Ok(v) if v > 0 => v,
                _ => continue,
            };
            let compression =
                compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1));
            let jpeg_tables = container
                .get_bytes(ifd_id, tags::JPEG_TABLES)
                .ok()
                .map(|b| b.to_vec());
            pyramid_ifds.push(VentanaPyramidIfdInfo {
                ifd_id,
                width,
                height,
                tile_width,
                tile_height,
                compression,
                jpeg_tables,
                description: desc,
            });
        }

        if pyramid_ifds.is_empty() {
            return Err(TiffParseError::Structure(
                "Ventana BIF: no tiled pyramid IFDs found".into(),
            ));
        }

        pyramid_ifds.sort_by(|a, b| {
            let area_a = u128::from(a.width) * u128::from(a.height);
            let area_b = u128::from(b.width) * u128::from(b.height);
            area_b.cmp(&area_a)
        });

        // Phase 3: Find level 0 XML (EncodeInfo) for public level-0 bounds.
        let level0_tile_width = pyramid_ifds[0].tile_width as i64;
        let level0_tile_height = pyramid_ifds[0].tile_height as i64;
        let encode_xml = find_encode_info_xml(container)?;
        let bif = parse_level0_xml(&encode_xml, level0_tile_width, level0_tile_height)?;

        if bif.areas.is_empty() {
            return Err(TiffParseError::Structure(
                "Ventana BIF: no scanned areas found in XML".into(),
            ));
        }

        // Phase 4: Build level 0 from the XML-driven irregular tile grid,
        // then keep the lower pyramid levels on the regular overview IFDs.
        let tile_advance_x = bif.tile_advance_x;
        let tile_advance_y = bif.tile_advance_y;
        if !tile_advance_x.is_finite()
            || !tile_advance_y.is_finite()
            || tile_advance_x <= 0.0
            || tile_advance_y <= 0.0
        {
            return Err(TiffParseError::Structure(format!(
                "Ventana: tile advance must be > 0 (got {}x{})",
                tile_advance_x, tile_advance_y
            )));
        }

        let mut level0_tiles: HashMap<(i64, i64), TileEntry> =
            HashMap::with_capacity(bif.tiles.len());
        let mut extra_top = 0u32;
        let mut extra_bottom = 0u32;
        let mut extra_left = 0u32;
        let mut extra_right = 0u32;
        for area in &bif.areas {
            let offset_x = area.x as f64 - area.start_col as f64 * bif.tile_advance_x;
            let offset_y = area.y as f64 - area.start_row as f64 * bif.tile_advance_y;
            let (area_extra_top, area_extra_bottom, area_extra_left, area_extra_right) =
                irregular_extra_tiles(
                    offset_x,
                    offset_y,
                    tile_advance_x,
                    tile_advance_y,
                    level0_tile_width as f64,
                    level0_tile_height as f64,
                );
            extra_top = extra_top.max(area_extra_top);
            extra_bottom = extra_bottom.max(area_extra_bottom);
            extra_left = extra_left.max(area_extra_left);
            extra_right = extra_right.max(area_extra_right);
        }

        for area in &bif.areas {
            let offset_x = area.x as f64 - area.start_col as f64 * bif.tile_advance_x;
            let offset_y = area.y as f64 - area.start_row as f64 * bif.tile_advance_y;
            let end_row = area.start_row.checked_add(area.tiles_down).ok_or_else(|| {
                TiffParseError::Structure("Ventana BIF: tile row range overflows".into())
            })?;
            let end_col = area
                .start_col
                .checked_add(area.tiles_across)
                .ok_or_else(|| {
                    TiffParseError::Structure("Ventana BIF: tile column range overflows".into())
                })?;
            for row in area.start_row..end_row {
                for col in area.start_col..end_col {
                    level0_tiles.insert(
                        (col, row),
                        TileEntry {
                            offset: (offset_x, offset_y),
                            dimensions: (pyramid_ifds[0].tile_width, pyramid_ifds[0].tile_height),
                            tiff_tile_index: None,
                        },
                    );
                }
            }
        }
        let level0_dims = ventana_level0_dimensions(&bif, level0_tile_width, level0_tile_height)?;

        let mut levels = Vec::with_capacity(pyramid_ifds.len());
        levels.push(Level {
            dimensions: level0_dims,
            downsample: 1.0,
            tile_layout: TileLayout::Irregular {
                tile_advance: (tile_advance_x, tile_advance_y),
                extra_tiles: (extra_top, extra_bottom, extra_left, extra_right),
                tiles: level0_tiles,
            },
        });

        let mut tile_sources = HashMap::with_capacity(pyramid_ifds.len());
        tile_sources.insert(
            TileSourceKey {
                scene: 0usize,
                series: 0usize,
                level: 0u32,
                z: 0,
                c: 0,
                t: 0,
            },
            TileSource::TiledIfd {
                ifd_id: pyramid_ifds[0].ifd_id,
                jpeg_tables: pyramid_ifds[0].jpeg_tables.clone(),
                compression: pyramid_ifds[0].compression,
            },
        );

        for (level_idx, info) in pyramid_ifds.iter().enumerate().skip(1) {
            let level_idx = u32::try_from(level_idx).map_err(|_| {
                TiffParseError::Structure("Ventana BIF: level index overflows u32".into())
            })?;
            let dims = ventana_public_level_dimensions(level0_dims, level_idx)?;
            let downsample = 1u64.checked_shl(level_idx).ok_or_else(|| {
                TiffParseError::Structure(format!(
                    "Ventana BIF: level {level_idx} downsample overflows"
                ))
            })?;
            let tiles_across = info.width.div_ceil(info.tile_width as u64);
            let tiles_down = info.height.div_ceil(info.tile_height as u64);
            levels.push(Level {
                dimensions: dims,
                downsample: downsample as f64,
                tile_layout: TileLayout::Regular {
                    tile_width: info.tile_width,
                    tile_height: info.tile_height,
                    tiles_across,
                    tiles_down,
                },
            });
            tile_sources.insert(
                TileSourceKey {
                    scene: 0usize,
                    series: 0usize,
                    level: level_idx,
                    z: 0,
                    c: 0,
                    t: 0,
                },
                TileSource::TiledIfd {
                    ifd_id: info.ifd_id,
                    jpeg_tables: info.jpeg_tables.clone(),
                    compression: info.compression,
                },
            );
        }

        if let Some(comment) = pyramid_ifds
            .first()
            .map(|info| info.description.as_str())
            .filter(|value| !value.is_empty())
        {
            properties.insert("openslide.comment", comment);
        }

        // Phase 5: Compute dataset ID.
        let property_ifd = pyramid_ifds
            .first()
            .map(|info| info.ifd_id)
            .ok_or_else(|| {
                TiffParseError::Structure("Ventana BIF: no pyramid IFDs found".into())
            })?;
        finish_single_scene_uint8_tiff_layout(
            container,
            pyramid_ifds.last().unwrap().ifd_id,
            property_ifd,
            AxesShape::default(),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            pyramid_ifds.iter().map(|ifd| ifd.ifd_id),
        )
    }
}

// ── Tiled IFD discovery ─────────────────────────────────────────────

struct VentanaPyramidIfdInfo {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    compression: Compression,
    jpeg_tables: Option<Vec<u8>>,
    description: String,
}

fn classify_associated_image(desc: &str) -> Option<String> {
    if desc.contains("thumbnail") {
        Some("thumbnail".to_string())
    } else if desc.contains("label image") || desc.contains("label_image") {
        Some("macro".to_string())
    } else {
        None
    }
}
