use super::*;

const STRIPPED_LEVEL_TILE_SIZE: u32 = 256;

impl TiffLayoutInterpreter for GenericTiffInterpreter {
    fn vendor_name(&self) -> &'static str {
        "generic-tiff"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        // Reject NDPI — handled by NdpiInterpreter.
        if container.is_ndpi() {
            return false;
        }

        // Reject obvious OME-TIFF: ImageDescription on first IFD contains
        // the OME XML namespace marker.
        if let Some(&first_id) = container.top_ifds().first() {
            if let Ok(desc) = container.get_string(first_id, tags::IMAGE_DESCRIPTION) {
                let lower = desc.to_ascii_lowercase();
                if lower.contains("<ome") || lower.contains("ome.xsd") {
                    return false;
                }
            }
        }

        // Accept any tiled TIFF, or one unambiguous strip-based RGB image.
        let has_tiled_ifd = container.top_ifds().iter().any(|&ifd_id| {
            container
                .ifd_by_id(ifd_id)
                .map(|ifd| ifd.tags.contains_key(&tags::TILE_WIDTH))
                .unwrap_or(false)
        });
        has_tiled_ifd
            || (container.top_ifds().len() == 1
                && is_supported_stripped_rgb_ifd(container, container.top_ifds()[0]))
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let mut tiled_ifds: Vec<TiledIfd> = Vec::new();
        let mut stripped_ifds: Vec<StrippedIfd> = Vec::new();

        // Phase 1: Walk all top-level IFDs and classify.
        for &ifd_id in container.top_ifds() {
            let ifd = container.ifd_by_id(ifd_id)?;

            let width = match container.get_u64(ifd_id, tags::IMAGE_WIDTH) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let height = match container.get_u64(ifd_id, tags::IMAGE_LENGTH) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if width == 0 || height == 0 {
                continue;
            }

            if ifd.tags.contains_key(&tags::TILE_WIDTH) {
                // Tiled IFD — pyramid level.
                let tile_width = container.get_u32(ifd_id, tags::TILE_WIDTH)?;
                let tile_height = container.get_u32(ifd_id, tags::TILE_LENGTH)?;
                let compression_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);

                tiled_ifds.push(TiledIfd {
                    ifd_id,
                    width,
                    height,
                    tile_width,
                    tile_height,
                    compression: compression_from_tag(compression_val),
                });
            } else {
                // Stripped IFD — associated image.
                let compression_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                let strip_offsets = container
                    .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                    .map(|values| values.to_vec())
                    .unwrap_or_default();
                let strip_byte_counts = container
                    .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                    .map(|values| values.to_vec())
                    .unwrap_or_default();

                stripped_ifds.push(StrippedIfd {
                    ifd_id,
                    width,
                    height,
                    compression: compression_from_tag(compression_val),
                    strip_offsets,
                    strip_byte_counts,
                });
            }
        }

        let stripped_level_index = (tiled_ifds.is_empty()
            && stripped_ifds.len() == 1
            && is_supported_stripped_rgb_ifd(container, stripped_ifds[0].ifd_id))
        .then_some(0usize);
        if tiled_ifds.is_empty() && stripped_level_index.is_none() {
            return Err(TiffParseError::Structure(
                "No tiled IFDs or supported stripped RGB image found in generic TIFF".into(),
            ));
        }

        // Phase 2: Sort tiled IFDs by area descending (largest = level 0).
        tiled_ifds.sort_by(|a, b| {
            let area_a = a.width * a.height;
            let area_b = b.width * b.height;
            area_b.cmp(&area_a)
        });

        let (base_w, base_h) = if let Some(first) = tiled_ifds.first() {
            (first.width, first.height)
        } else {
            let stripped = &stripped_ifds[stripped_level_index.unwrap()];
            (stripped.width, stripped.height)
        };

        // Phase 3: JPEG tables from tag 347 on first tiled IFD if present.
        let jpeg_tables: Option<Vec<u8>> = tiled_ifds.first().and_then(|first| {
            container
                .get_bytes(first.ifd_id, tags::JPEG_TABLES)
                .ok()
                .map(|bytes| bytes.to_vec())
        });

        // Phase 4: Build levels and tile sources.
        let mut levels = Vec::with_capacity(tiled_ifds.len());
        let mut tile_sources = HashMap::new();

        for (level_idx, tifd) in tiled_ifds.iter().enumerate() {
            let downsample = if level_idx == 0 {
                1.0
            } else {
                let dw = base_w as f64 / tifd.width as f64;
                let dh = base_h as f64 / tifd.height as f64;
                (dw + dh) / 2.0
            };

            levels.push(regular_tiff_level(
                "Generic TIFF",
                tifd.width,
                tifd.height,
                tifd.tile_width,
                tifd.tile_height,
                downsample,
            )?);

            let key = TileSourceKey {
                scene: 0usize,
                series: 0usize,
                level: level_idx as u32,
                z: 0,
                c: 0,
                t: 0,
            };
            tile_sources.insert(
                key,
                TileSource::TiledIfd {
                    ifd_id: tifd.ifd_id,
                    jpeg_tables: jpeg_tables.clone(),
                    compression: tifd.compression,
                },
            );
        }

        if let Some(index) = stripped_level_index {
            let stripped = &stripped_ifds[index];
            levels.push(regular_tiff_level(
                "Generic stripped TIFF",
                stripped.width,
                stripped.height,
                STRIPPED_LEVEL_TILE_SIZE,
                STRIPPED_LEVEL_TILE_SIZE,
                1.0,
            )?);
            tile_sources.insert(
                TileSourceKey {
                    scene: 0,
                    series: 0,
                    level: 0,
                    z: 0,
                    c: 0,
                    t: 0,
                },
                TileSource::StrippedLevel {
                    ifd_id: stripped.ifd_id,
                    compression: stripped.compression,
                    strip_offsets: stripped.strip_offsets.clone(),
                    strip_byte_counts: stripped.strip_byte_counts.clone(),
                },
            );
        }

        // Phase 5: Build associated images from stripped IFDs.
        let mut associated_images: HashMap<String, AssociatedImage> = HashMap::new();
        let mut associated_sources: HashMap<String, TileSource> = HashMap::new();

        for (i, sifd) in stripped_ifds.iter().enumerate() {
            if stripped_level_index == Some(i) {
                continue;
            }
            let name = format!("image_{}", i);
            associated_images.insert(
                name.clone(),
                AssociatedImage {
                    dimensions: (
                        u32::try_from(sifd.width).unwrap_or(u32::MAX),
                        u32::try_from(sifd.height).unwrap_or(u32::MAX),
                    ),
                    sample_type: SampleType::Uint8,
                    channels: 3,
                },
            );
            associated_sources.insert(
                name,
                TileSource::Stripped {
                    ifd_id: sifd.ifd_id,
                    jpeg_tables: None,
                    compression: sifd.compression,
                    strip_offsets: sifd.strip_offsets.clone(),
                    strip_byte_counts: sifd.strip_byte_counts.clone(),
                },
            );
        }

        // Phase 6: Properties.
        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "generic-tiff");

        if let Some(&first_id) = container.top_ifds().first() {
            if let Ok(desc) = container.get_string(first_id, tags::IMAGE_DESCRIPTION) {
                properties.insert("openslide.comment", desc.to_string());
            }

            // Extract MPP from TIFF XResolution / YResolution tags.
            // ResolutionUnit: 2 = inch (default), 3 = centimeter.
            let res_unit = container
                .get_u32(first_id, tags::RESOLUTION_UNIT)
                .unwrap_or(2); // default: inch
            let unit_to_microns = match res_unit {
                3 => 10_000.0, // 1 cm = 10,000 µm
                _ => 25_400.0, // 1 inch = 25,400 µm
            };
            if let Ok(x_res) = container.get_f64(first_id, tags::X_RESOLUTION) {
                if x_res > 0.0 {
                    let mpp_x = unit_to_microns / x_res;
                    properties.insert("openslide.mpp-x", format!("{mpp_x:.6}"));
                }
            }
            if let Ok(y_res) = container.get_f64(first_id, tags::Y_RESOLUTION) {
                if y_res > 0.0 {
                    let mpp_y = unit_to_microns / y_res;
                    properties.insert("openslide.mpp-y", format!("{mpp_y:.6}"));
                }
            }
        }

        // Phase 7: Dataset identity from TIFF quickhash-compatible content hashing.
        let property_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("No IFDs in generic TIFF container".into()))?;
        // Phase 8: Assemble Dataset with single Scene, single Series.
        let (lowest_resolution_ifd, source_icc_ifds) = if let Some(index) = stripped_level_index {
            let ifd_id = stripped_ifds[index].ifd_id;
            (ifd_id, vec![ifd_id])
        } else {
            (
                tiled_ifds.last().unwrap().ifd_id,
                tiled_ifds.iter().map(|ifd| ifd.ifd_id).collect(),
            )
        };
        finish_single_scene_uint8_tiff_layout(
            container,
            lowest_resolution_ifd,
            property_ifd,
            AxesShape::default(),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            source_icc_ifds,
        )
    }
}

struct TiledIfd {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    compression: Compression,
}

struct StrippedIfd {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    compression: Compression,
    strip_offsets: Vec<u64>,
    strip_byte_counts: Vec<u64>,
}
