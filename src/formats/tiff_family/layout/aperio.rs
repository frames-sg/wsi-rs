//! Aperio SVS layout interpreter.
//!
//! Classifies IFDs from an Aperio SVS TiffContainer into pyramid levels
//! (tiled IFDs) and associated images (stripped IFDs). Produces a
//! DatasetLayout with TileSource descriptors for each plane.

use std::collections::HashMap;

use crate::core::types::*;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::{
    compression_from_tag, finish_single_scene_uint8_tiff_layout, regular_tiff_level, DatasetLayout,
    TiffLayoutInterpreter, TileSource, TileSourceKey,
};

// ── AperioInterpreter ────────────────────────────────────────────

pub(crate) struct AperioInterpreter;

/// Intermediate: a tiled IFD classified as a pyramid level.
struct TiledIfd {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    compression: Compression,
}

/// Intermediate: a stripped IFD classified as an associated image.
struct StrippedIfd {
    ifd_id: IfdId,
    ifd_index: usize,
    width: u32,
    height: u32,
    compression: Compression,
    strip_offsets: Vec<u64>,
    strip_byte_counts: Vec<u64>,
}

impl TiffLayoutInterpreter for AperioInterpreter {
    fn vendor_name(&self) -> &'static str {
        "aperio"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        let first_id = match container.top_ifds().first() {
            Some(&id) => id,
            None => return false,
        };

        // First top-level IFD must have TILE_WIDTH tag
        let ifd = match container.ifd_by_id(first_id) {
            Ok(ifd) => ifd,
            Err(_) => return false,
        };
        if !ifd.tags.contains_key(&tags::TILE_WIDTH) {
            return false;
        }

        // ImageDescription must start with "Aperio"
        match container.get_string(first_id, tags::IMAGE_DESCRIPTION) {
            Ok(desc) => desc.starts_with("Aperio"),
            Err(_) => false,
        }
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let mut tiled_ifds: Vec<TiledIfd> = Vec::new();
        let mut stripped_ifds: Vec<StrippedIfd> = Vec::new();

        // Phase 1: Classify each top-level IFD as tiled (pyramid) or stripped (associated)
        for (idx, &ifd_id) in container.top_ifds().iter().enumerate() {
            let ifd = container.ifd_by_id(ifd_id)?;

            if ifd.tags.contains_key(&tags::TILE_WIDTH) {
                // Tiled IFD → pyramid level
                let width = container.get_u64(ifd_id, tags::IMAGE_WIDTH)?;
                let height = container.get_u64(ifd_id, tags::IMAGE_LENGTH)?;
                let tile_width = container.get_u32(ifd_id, tags::TILE_WIDTH)?;
                let tile_height = container.get_u32(ifd_id, tags::TILE_LENGTH)?;
                let comp_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                let compression = compression_from_tag(comp_val);

                tiled_ifds.push(TiledIfd {
                    ifd_id,
                    width,
                    height,
                    tile_width,
                    tile_height,
                    compression,
                });
            } else {
                // Stripped IFD → associated image
                let width =
                    u32::try_from(container.get_u64(ifd_id, tags::IMAGE_WIDTH).unwrap_or(0))
                        .unwrap_or(u32::MAX);
                let height =
                    u32::try_from(container.get_u64(ifd_id, tags::IMAGE_LENGTH).unwrap_or(0))
                        .unwrap_or(u32::MAX);
                let comp_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                let compression = compression_from_tag(comp_val);
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
                    ifd_index: idx,
                    width,
                    height,
                    compression,
                    strip_offsets,
                    strip_byte_counts,
                });
            }
        }

        if tiled_ifds.is_empty() {
            return Err(TiffParseError::Structure(
                "No tiled pyramid levels found in Aperio SVS".into(),
            ));
        }

        // Phase 2: Sort tiled IFDs by area descending (largest = level 0)
        tiled_ifds.sort_by(|a, b| {
            let area_a = a.width * a.height;
            let area_b = b.width * b.height;
            area_b.cmp(&area_a)
        });

        // Some Aperio slides store different JPEG tables per pyramid level.
        let base_jpeg_tables = container
            .get_bytes(tiled_ifds[0].ifd_id, tags::JPEG_TABLES)
            .ok()
            .map(|b| b.to_vec());

        // Build levels and tile sources
        let base_w = tiled_ifds[0].width as f64;
        let base_h = tiled_ifds[0].height as f64;

        let mut levels = Vec::new();
        let mut tile_sources = HashMap::new();

        for (level_idx, tifd) in tiled_ifds.iter().enumerate() {
            let downsample = if level_idx == 0 {
                1.0
            } else {
                let dw = base_w / tifd.width as f64;
                let dh = base_h / tifd.height as f64;
                (dw + dh) / 2.0
            };

            levels.push(regular_tiff_level(
                "Aperio",
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
            let jpeg_tables = if tifd.compression == Compression::Jpeg {
                container
                    .get_bytes(tifd.ifd_id, tags::JPEG_TABLES)
                    .ok()
                    .map(|bytes| bytes.to_vec())
                    .or_else(|| base_jpeg_tables.clone())
            } else {
                None
            };
            tile_sources.insert(
                key,
                TileSource::TiledIfd {
                    ifd_id: tifd.ifd_id,
                    jpeg_tables,
                    compression: tifd.compression,
                },
            );
        }

        // Phase 3: Classify stripped IFDs as associated images
        let mut associated_images: HashMap<String, AssociatedImage> = HashMap::new();
        let mut associated_sources: HashMap<String, TileSource> = HashMap::new();

        for sifd in &stripped_ifds {
            if sifd.width == 0 || sifd.height == 0 {
                continue;
            }

            let name = if sifd.ifd_index == 1 {
                "thumbnail".to_string()
            } else {
                // Check ImageDescription for "label" or "macro"
                container
                    .get_string(sifd.ifd_id, tags::IMAGE_DESCRIPTION)
                    .ok()
                    .and_then(|desc| {
                        let lower = desc.to_lowercase();
                        if lower.contains("label") {
                            Some("label".to_string())
                        } else if lower.contains("macro") {
                            Some("macro".to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| format!("image_{}", sifd.ifd_index))
            };

            let spp = container
                .get_u32(sifd.ifd_id, tags::SAMPLES_PER_PIXEL)
                .unwrap_or(3) as u16;

            associated_images.insert(
                name.clone(),
                AssociatedImage {
                    dimensions: (sifd.width, sifd.height),
                    sample_type: SampleType::Uint8,
                    channels: spp,
                },
            );
            associated_sources.insert(
                name,
                TileSource::Stripped {
                    ifd_id: sifd.ifd_id,
                    jpeg_tables: if sifd.compression == Compression::Jpeg {
                        container
                            .get_bytes(sifd.ifd_id, tags::JPEG_TABLES)
                            .ok()
                            .map(|bytes| bytes.to_vec())
                            .or_else(|| base_jpeg_tables.clone())
                    } else {
                        None
                    },
                    compression: sifd.compression,
                    strip_offsets: sifd.strip_offsets.clone(),
                    strip_byte_counts: sifd.strip_byte_counts.clone(),
                },
            );
        }

        // Phase 4: Parse properties from ImageDescription
        let properties = self.parse_properties(container)?;

        // Phase 5: Compute dataset ID
        let property_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("No IFDs in Aperio container".into()))?;
        finish_single_scene_uint8_tiff_layout(
            container,
            tiled_ifds.last().unwrap().ifd_id,
            property_ifd,
            AxesShape::default(),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            tiled_ifds.iter().map(|ifd| ifd.ifd_id),
        )
    }
}

impl AperioInterpreter {
    /// Parse Aperio's pipe-delimited ImageDescription into properties.
    ///
    /// Format: `"Aperio Image Library ...|Key1 = Value1|Key2 = Value2|..."`
    ///
    /// Segments after the first are split on `=` and stored as `aperio.{key}`.
    /// Standard compatibility properties are mapped from the Aperio-specific keys.
    fn parse_properties(&self, container: &TiffContainer) -> Result<Properties, TiffParseError> {
        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "aperio");

        let first_ifd = match container.top_ifds().first() {
            Some(&id) => id,
            None => return Ok(properties),
        };

        // Parse pipe-delimited ImageDescription
        if let Ok(desc) = container.get_string(first_ifd, tags::IMAGE_DESCRIPTION) {
            // Store raw description as openslide.comment
            properties.insert("openslide.comment", desc.to_string());

            // Split by '|', skip first segment (the "Aperio Image Library ..." prefix)
            let parts: Vec<&str> = desc.split('|').collect();
            for part in parts.iter().skip(1) {
                if let Some((key, value)) = part.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();
                    if !key.is_empty() && !value.is_empty() {
                        properties.insert(format!("aperio.{}", key), value.to_string());
                    }
                }
            }
        }

        // Map standard compatibility properties from Aperio keys.
        if let Some(mag) = properties.get("aperio.AppMag").map(|s| s.to_string()) {
            properties.insert("openslide.objective-power", mag);
        }
        if let Some(mpp) = properties.get("aperio.MPP").map(|s| s.to_string()) {
            properties.insert("openslide.mpp-x", mpp.clone());
            properties.insert("openslide.mpp-y", mpp);
        }

        Ok(properties)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::tiff_family::container::TiffContainer;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ── Synthetic TIFF builder ───────────────────────────────────────

    /// Represents one tag to write into a synthetic IFD.
    /// For out-of-line data (ASCII strings, byte arrays), use `ool_data`.
    struct SyntheticTag {
        tag: u16,
        tiff_type: u16,
        count: u32,
        /// Inline value (up to 4 bytes). Ignored when `ool_data` is Some.
        inline_value: [u8; 4],
        /// Out-of-line data. When present, the tag's value/offset field
        /// is patched to point to this data appended after all IFDs.
        ool_data: Option<Vec<u8>>,
    }

    impl SyntheticTag {
        fn long(tag: u16, value: u32) -> Self {
            SyntheticTag {
                tag,
                tiff_type: 4, // LONG
                count: 1,
                inline_value: value.to_le_bytes(),
                ool_data: None,
            }
        }

        fn short(tag: u16, value: u16) -> Self {
            let mut bytes = [0u8; 4];
            bytes[0..2].copy_from_slice(&value.to_le_bytes());
            SyntheticTag {
                tag,
                tiff_type: 3, // SHORT
                count: 1,
                inline_value: bytes,
                ool_data: None,
            }
        }

        fn ascii(tag: u16, text: &str) -> Self {
            let mut data = text.as_bytes().to_vec();
            data.push(0); // null terminator
            SyntheticTag {
                tag,
                tiff_type: 2, // ASCII
                count: data.len() as u32,
                inline_value: [0; 4],
                ool_data: Some(data),
            }
        }

        fn bytes(tag: u16, data: Vec<u8>) -> Self {
            SyntheticTag {
                tag,
                tiff_type: 7, // UNDEFINED
                count: data.len() as u32,
                inline_value: [0; 4],
                ool_data: Some(data),
            }
        }
    }

    /// Build a synthetic classic TIFF file with chained top-level IFDs.
    /// Supports both inline and out-of-line tag data.
    fn build_aperio_tiff(ifds: &[Vec<SyntheticTag>]) -> NamedTempFile {
        let mut buf = Vec::new();

        // TIFF header: little-endian, classic TIFF
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        let first_ifd_offset_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes()); // placeholder

        // First pass: write out-of-line data blocks and record their offsets.
        // We accumulate (ifd_idx, tag_idx, file_offset) tuples.
        let mut ool_offsets: Vec<(usize, usize, u32)> = Vec::new();
        for (ifd_idx, tags) in ifds.iter().enumerate() {
            for (tag_idx, tag) in tags.iter().enumerate() {
                if let Some(data) = &tag.ool_data {
                    let offset = buf.len() as u32;
                    buf.extend_from_slice(data);
                    ool_offsets.push((ifd_idx, tag_idx, offset));
                }
            }
        }

        // Second pass: write IFDs
        let mut ifd_offsets: Vec<u32> = Vec::new();
        let mut next_ifd_patch_positions: Vec<usize> = Vec::new();

        for (ifd_idx, tags) in ifds.iter().enumerate() {
            let ifd_offset = buf.len() as u32;
            ifd_offsets.push(ifd_offset);

            // Sort tags by ID (TIFF spec requirement)
            let mut sorted: Vec<(usize, &SyntheticTag)> = tags.iter().enumerate().collect();
            sorted.sort_by_key(|(_, t)| t.tag);

            let entry_count = sorted.len() as u16;
            buf.extend_from_slice(&entry_count.to_le_bytes());

            for (orig_idx, tag) in &sorted {
                buf.extend_from_slice(&tag.tag.to_le_bytes());
                buf.extend_from_slice(&tag.tiff_type.to_le_bytes());
                buf.extend_from_slice(&tag.count.to_le_bytes());

                if tag.ool_data.is_some() {
                    // Find the offset we recorded
                    let offset = ool_offsets
                        .iter()
                        .find(|(ii, ti, _)| *ii == ifd_idx && *ti == *orig_idx)
                        .map(|(_, _, o)| *o)
                        .unwrap();
                    buf.extend_from_slice(&offset.to_le_bytes());
                } else {
                    buf.extend_from_slice(&tag.inline_value);
                }
            }

            // Next IFD offset (classic TIFF: 4 bytes)
            let next_pos = buf.len();
            buf.extend_from_slice(&0u32.to_le_bytes());
            next_ifd_patch_positions.push(next_pos);
        }

        // Patch first IFD offset
        let first_offset = ifd_offsets[0].to_le_bytes();
        buf[first_ifd_offset_pos..first_ifd_offset_pos + 4].copy_from_slice(&first_offset);

        // Chain IFDs
        for i in 0..ifd_offsets.len().saturating_sub(1) {
            let next = ifd_offsets[i + 1].to_le_bytes();
            let pos = next_ifd_patch_positions[i];
            buf[pos..pos + 4].copy_from_slice(&next);
        }

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file
    }

    // ── Detection tests ──────────────────────────────────────────────

    #[test]
    fn detect_aperio_svs() {
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        assert!(interpreter.detect(&container));
    }

    #[test]
    fn reject_non_aperio_tiled() {
        // Tiled but ImageDescription doesn't start with "Aperio"
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Generic TIFF"),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        assert!(!interpreter.detect(&container));
    }

    #[test]
    fn reject_stripped_aperio_description() {
        // Has "Aperio" in description but no TILE_WIDTH tag
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        assert!(!interpreter.detect(&container));
    }

    #[test]
    fn reject_no_description() {
        // Tiled but no ImageDescription tag at all
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        assert!(!interpreter.detect(&container));
    }

    // ── Interpretation tests ─────────────────────────────────────────

    #[test]
    fn interpret_single_level() {
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7), // JPEG
            SyntheticTag::ascii(
                tags::IMAGE_DESCRIPTION,
                "Aperio Image Library v1.0|AppMag = 40|MPP = 0.25",
            ),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert_eq!(layout.dataset.scenes.len(), 1);
        let series = &layout.dataset.scenes[0].series[0];
        assert_eq!(series.levels.len(), 1);
        assert_eq!(series.levels[0].dimensions, (4096, 3072));
        assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

        // Tile layout
        match &series.levels[0].tile_layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across,
                tiles_down,
            } => {
                assert_eq!(*tile_width, 256);
                assert_eq!(*tile_height, 256);
                assert_eq!(*tiles_across, 16); // 4096/256
                assert_eq!(*tiles_down, 12); // 3072/256
            }
            other => panic!("expected Regular, got: {:?}", other),
        }

        // Tile source
        let key = TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 0u32,
            z: 0,
            c: 0,
            t: 0,
        };
        assert!(layout.tile_sources.contains_key(&key));
        match layout.tile_sources.get(&key).unwrap() {
            TileSource::TiledIfd { compression, .. } => {
                assert_eq!(*compression, Compression::Jpeg);
            }
            other => panic!("expected TiledIfd, got: {:?}", other),
        }
    }

    #[test]
    fn interpret_populates_source_and_legacy_icc_from_tiff_tag() {
        let icc_bytes = vec![0, 1, 2, 3, 0, 255];
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            SyntheticTag::bytes(tags::ICC_PROFILE, icc_bytes.clone()),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let ifd_id = container.top_ifds()[0];
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert_eq!(layout.dataset.source_icc_profiles.len(), 1);
        let profile = &layout.dataset.source_icc_profiles[0];
        assert_eq!(profile.bytes, icc_bytes);
        assert_eq!(
            layout
                .dataset
                .icc_profiles
                .get(&IccProfileKey::new(SceneId::new(0), SeriesId::new(0))),
            Some(&icc_bytes)
        );
        assert_eq!(
            profile.provenance,
            IccProfileProvenance::TiffTag {
                ifd_id: ifd_id.0,
                tag: tags::ICC_PROFILE,
            }
        );
    }

    #[test]
    fn interpret_ignores_associated_only_icc_for_main_series() {
        let associated_icc = vec![9, 8, 7, 6, 5];
        let file = build_aperio_tiff(&[
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            ],
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 400),
                SyntheticTag::long(tags::IMAGE_LENGTH, 300),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::long(tags::STRIP_OFFSETS, 100),
                SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 5000),
                SyntheticTag::bytes(tags::ICC_PROFILE, associated_icc),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert!(layout.dataset.source_icc_profiles.is_empty());
        assert!(layout.dataset.icc_profiles.is_empty());
        assert!(layout.dataset.associated_images.contains_key("thumbnail"));
    }

    #[test]
    fn interpret_multi_level_sorted_by_area() {
        let file = build_aperio_tiff(&[
            // IFD 0: large (level 0 after sorting)
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            ],
            // IFD 1: smaller (level 1 after sorting)
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
                SyntheticTag::long(tags::IMAGE_LENGTH, 768),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        let series = &layout.dataset.scenes[0].series[0];
        assert_eq!(series.levels.len(), 2);

        // Level 0 = largest
        assert_eq!(series.levels[0].dimensions, (4096, 3072));
        assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

        // Level 1 = smaller, downsample ~4.0
        assert_eq!(series.levels[1].dimensions, (1024, 768));
        assert!(series.levels[1].downsample > 3.5);
        assert!(series.levels[1].downsample < 4.5);
    }

    #[test]
    fn interpret_multi_level_reverse_order() {
        // Small IFD first in chain, large IFD second — should still sort correctly
        let file = build_aperio_tiff(&[
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 512),
                SyntheticTag::long(tags::IMAGE_LENGTH, 384),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            ],
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        let series = &layout.dataset.scenes[0].series[0];
        // Largest first regardless of IFD chain order
        assert_eq!(series.levels[0].dimensions, (4096, 3072));
        assert_eq!(series.levels[1].dimensions, (512, 384));
    }

    #[test]
    fn interpret_tiles_across_rounds_up() {
        // 4100 / 256 = 16.015... → tiles_across = 17
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4100),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        match &layout.dataset.scenes[0].series[0].levels[0].tile_layout {
            TileLayout::Regular { tiles_across, .. } => {
                assert_eq!(*tiles_across, 17);
            }
            other => panic!("expected Regular, got: {:?}", other),
        }
    }

    #[test]
    fn interpret_no_tiled_ifds_returns_error() {
        // All stripped — no pyramid levels
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 800),
            SyntheticTag::long(tags::IMAGE_LENGTH, 600),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let result = interpreter.interpret(&container);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("No tiled pyramid levels"),
            "expected 'No tiled pyramid levels', got: {}",
            msg,
        );
    }

    // ── Associated image tests ───────────────────────────────────────

    #[test]
    fn interpret_thumbnail_at_index_1() {
        let file = build_aperio_tiff(&[
            // IFD 0: tiled pyramid
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            ],
            // IFD 1: stripped → "thumbnail"
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 400),
                SyntheticTag::long(tags::IMAGE_LENGTH, 300),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::long(tags::STRIP_OFFSETS, 100),
                SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 5000),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert!(layout.dataset.associated_images.contains_key("thumbnail"));
        let thumb = &layout.dataset.associated_images["thumbnail"];
        assert_eq!(thumb.dimensions, (400, 300));

        assert!(layout.associated_sources.contains_key("thumbnail"));
        match layout.associated_sources.get("thumbnail").unwrap() {
            TileSource::Stripped {
                strip_offsets,
                strip_byte_counts,
                ..
            } => {
                assert_eq!(strip_offsets.as_slice(), &[100]);
                assert_eq!(strip_byte_counts.as_slice(), &[5000]);
            }
            other => panic!("expected Stripped, got: {:?}", other),
        }
    }

    #[test]
    fn interpret_label_and_macro_by_description() {
        let file = build_aperio_tiff(&[
            // IFD 0: tiled pyramid
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            ],
            // IFD 1: thumbnail (stripped, index 1)
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 400),
                SyntheticTag::long(tags::IMAGE_LENGTH, 300),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::long(tags::STRIP_OFFSETS, 100),
                SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 5000),
            ],
            // IFD 2: label (stripped)
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 200),
                SyntheticTag::long(tags::IMAGE_LENGTH, 100),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::long(tags::STRIP_OFFSETS, 200),
                SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 2000),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "label image"),
            ],
            // IFD 3: macro (stripped)
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 800),
                SyntheticTag::long(tags::IMAGE_LENGTH, 600),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::long(tags::STRIP_OFFSETS, 300),
                SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 10000),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "macro scan"),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert!(layout.dataset.associated_images.contains_key("thumbnail"));
        assert!(layout.dataset.associated_images.contains_key("label"));
        assert!(layout.dataset.associated_images.contains_key("macro"));
        assert_eq!(
            layout.dataset.associated_images["label"].dimensions,
            (200, 100)
        );
        assert_eq!(
            layout.dataset.associated_images["macro"].dimensions,
            (800, 600)
        );
    }

    #[test]
    fn interpret_stripped_fallback_name() {
        // IFD at index 2 with no recognized description → "image_2"
        let file = build_aperio_tiff(&[
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            ],
            // IFD 1: tiled (another pyramid level)
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
                SyntheticTag::long(tags::IMAGE_LENGTH, 768),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
            ],
            // IFD 2: stripped with unknown description
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 100),
                SyntheticTag::long(tags::IMAGE_LENGTH, 50),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::long(tags::STRIP_OFFSETS, 100),
                SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 1000),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "something else"),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert!(layout.dataset.associated_images.contains_key("image_2"));
    }

    // ── Property parsing tests ───────────────────────────────────────

    #[test]
    fn properties_vendor_and_comment() {
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(
                tags::IMAGE_DESCRIPTION,
                "Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528",
            ),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert_eq!(layout.dataset.properties.vendor(), Some("aperio"));
        assert_eq!(
            layout.dataset.properties.get("openslide.comment"),
            Some("Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528"),
        );
    }

    #[test]
    fn properties_aperio_keys_parsed() {
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(
                tags::IMAGE_DESCRIPTION,
                "Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528|StripeWidth = 1000",
            ),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert_eq!(layout.dataset.properties.get("aperio.AppMag"), Some("40"));
        assert_eq!(layout.dataset.properties.get("aperio.MPP"), Some("0.2528"));
        assert_eq!(
            layout.dataset.properties.get("aperio.StripeWidth"),
            Some("1000"),
        );
    }

    #[test]
    fn properties_objective_power_and_mpp() {
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(
                tags::IMAGE_DESCRIPTION,
                "Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528",
            ),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        assert_eq!(
            layout.dataset.properties.get("openslide.objective-power"),
            Some("40"),
        );
        assert_eq!(
            layout.dataset.properties.get("openslide.mpp-x"),
            Some("0.2528"),
        );
        assert_eq!(
            layout.dataset.properties.get("openslide.mpp-y"),
            Some("0.2528"),
        );

        // Verify via convenience accessors
        assert!((layout.dataset.properties.objective_power().unwrap() - 40.0).abs() < 0.001);
        let (mpp_x, mpp_y) = layout.dataset.properties.mpp().unwrap();
        assert!((mpp_x - 0.2528).abs() < 0.0001);
        assert!((mpp_y - 0.2528).abs() < 0.0001);
    }

    // ── JPEG tables test ─────────────────────────────────────────────

    #[test]
    fn jpeg_tables_propagated_to_tile_source() {
        let fake_tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43]; // minimal JPEG header fragment
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            SyntheticTag::bytes(tags::JPEG_TABLES, fake_tables.clone()),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        let key = TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 0u32,
            z: 0,
            c: 0,
            t: 0,
        };
        match layout.tile_sources.get(&key).unwrap() {
            TileSource::TiledIfd { jpeg_tables, .. } => {
                assert!(jpeg_tables.is_some());
                assert_eq!(jpeg_tables.as_ref().unwrap(), &fake_tables);
            }
            other => panic!("expected TiledIfd, got: {:?}", other),
        }
    }

    #[test]
    fn jpeg_tables_are_kept_per_pyramid_ifd() {
        let level0_tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
        let level1_tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x01];
        let file = build_aperio_tiff(&[
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
                SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
                SyntheticTag::bytes(tags::JPEG_TABLES, level0_tables.clone()),
            ],
            vec![
                SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
                SyntheticTag::long(tags::IMAGE_LENGTH, 768),
                SyntheticTag::long(tags::TILE_WIDTH, 256),
                SyntheticTag::long(tags::TILE_LENGTH, 256),
                SyntheticTag::short(tags::COMPRESSION, 7),
                SyntheticTag::ascii(
                    tags::IMAGE_DESCRIPTION,
                    "Aperio Image Library v1.0 -> 1024x768 JPEG/RGB",
                ),
                SyntheticTag::bytes(tags::JPEG_TABLES, level1_tables.clone()),
            ],
        ]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout = interpreter.interpret(&container).unwrap();

        for (level, expected) in [(0, level0_tables), (1, level1_tables)] {
            let key = TileSourceKey {
                scene: 0usize,
                series: 0usize,
                level,
                z: 0,
                c: 0,
                t: 0,
            };
            match layout.tile_sources.get(&key).unwrap() {
                TileSource::TiledIfd { jpeg_tables, .. } => {
                    assert_eq!(jpeg_tables.as_ref(), Some(&expected));
                }
                other => panic!("expected TiledIfd, got: {:?}", other),
            }
        }
    }

    // ── Dataset ID test ──────────────────────────────────────────────

    #[test]
    fn dataset_id_deterministic() {
        let file = build_aperio_tiff(&[vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ]]);

        let container = TiffContainer::open(file.path()).unwrap();
        let interpreter = AperioInterpreter;
        let layout1 = interpreter.interpret(&container).unwrap();
        let layout2 = interpreter.interpret(&container).unwrap();
        assert_eq!(layout1.dataset.id, layout2.dataset.id);
    }

    // ── Compression mapping tests ────────────────────────────────────

    #[test]
    fn compression_from_tag_values() {
        assert_eq!(compression_from_tag(1), Compression::None);
        assert_eq!(compression_from_tag(6), Compression::Jpeg);
        assert_eq!(compression_from_tag(7), Compression::Jpeg);
        assert_eq!(compression_from_tag(33003), Compression::Jp2kYcbcr);
        assert_eq!(compression_from_tag(33005), Compression::Jp2kYcbcr);
        assert_eq!(compression_from_tag(33004), Compression::Jp2kRgb);
        assert_eq!(compression_from_tag(9999), Compression::Other(9999));
    }
}
