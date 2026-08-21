//! NDPI layout interpreter.
//!
//! Classifies IFDs from an NDPI TiffContainer into pyramid levels,
//! associated images, and z-stack planes. Produces a DatasetLayout
//! with TileSource descriptors for each plane.

use std::collections::HashMap;

use crate::core::types::*;
use crate::decode::jpeg::is_sof_marker;
use crate::formats::tiff_family::container::tags;
use crate::formats::tiff_family::container::TiffContainer;
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;
use j2k_jpeg::Decoder as J2kJpegDecoder;

use super::{
    compression_from_tag, finish_single_scene_uint8_tiff_layout, DatasetLayout,
    TiffLayoutInterpreter, TileSource, TileSourceKey,
};

// ── NDPI-specific tag constants ───────────────────────────────────

const NDPI_SOURCELENS: u16 = 65421;
const NDPI_XOFFSET: u16 = 65422;
const NDPI_YOFFSET: u16 = 65423;
const NDPI_FOCAL_PLANE: u16 = 65424;
const NDPI_MCU_STARTS: u16 = 65426;
const NDPI_REFERENCE: u16 = 65427;
const NDPI_PROPERTY_MAP: u16 = 65449;
const JPEG_HEADER_PROBE_BYTES: u64 = 4096;

#[derive(Clone)]
struct JpegGeometryProbe {
    header: Vec<u8>,
    restart_interval: u16,
    mcu_w: u32,
    mcu_h: u32,
}

fn probe_jpeg_geometry_via_j2k(
    container: &TiffContainer,
    ifd_id: IfdId,
) -> Result<JpegGeometryProbe, TiffParseError> {
    let strip_offset = container.get_u64(ifd_id, tags::STRIP_OFFSETS)?;
    let strip_byte_count = container.get_u64(ifd_id, tags::STRIP_BYTE_COUNTS)?;
    let read_len = JPEG_HEADER_PROBE_BYTES.min(strip_byte_count);
    let header = container.pread(strip_offset, read_len)?;
    probe_jpeg_geometry_bytes_via_j2k(header)
}

fn probe_jpeg_geometry_bytes_via_j2k(header: Vec<u8>) -> Result<JpegGeometryProbe, TiffParseError> {
    match J2kJpegDecoder::inspect(&header) {
        Ok(info) => Ok(JpegGeometryProbe {
            header: jpeg_header_prefix(&header)?.to_vec(),
            restart_interval: info.restart_interval.unwrap_or(0),
            mcu_w: info.mcu_geometry.width,
            mcu_h: info.mcu_geometry.height,
        }),
        Err(inspect_err) => {
            let probe = probe_jpeg_geometry_bytes_lenient(&header).map_err(|lenient_err| {
                TiffParseError::Structure(format!(
                    "cannot parse JPEG geometry with j2k: {inspect_err}; lenient NDPI probe failed: {lenient_err}"
                ))
            })?;
            Ok(probe)
        }
    }
}

fn probe_jpeg_geometry_bytes_lenient(header: &[u8]) -> Result<JpegGeometryProbe, TiffParseError> {
    if header.len() < 2 || header[0..2] != [0xFF, 0xD8] {
        return Err(TiffParseError::Structure(
            "NDPI JPEG header missing SOI".into(),
        ));
    }

    let mut restart_interval = 0;
    let mut mcu_w = None;
    let mut mcu_h = None;
    let mut i = 2usize;

    while i + 1 < header.len() {
        if header[i] != 0xFF {
            return Err(TiffParseError::Structure(format!(
                "NDPI JPEG marker expected at byte {i}"
            )));
        }
        while i < header.len() && header[i] == 0xFF {
            i += 1;
        }
        if i >= header.len() {
            break;
        }
        let marker = header[i];
        i += 1;

        match marker {
            0xD9 => break,
            0xDA => {
                let prefix = jpeg_header_prefix(header)?;
                let mcu_w = mcu_w.ok_or_else(|| {
                    TiffParseError::Structure("NDPI JPEG header missing SOF marker".into())
                })?;
                let mcu_h = mcu_h.ok_or_else(|| {
                    TiffParseError::Structure("NDPI JPEG header missing SOF marker".into())
                })?;
                return Ok(JpegGeometryProbe {
                    header: prefix.to_vec(),
                    restart_interval,
                    mcu_w,
                    mcu_h,
                });
            }
            0x00 | 0xD0..=0xD7 => continue,
            _ => {}
        }

        if i + 1 >= header.len() {
            return Err(TiffParseError::Structure(format!(
                "NDPI JPEG marker FF{marker:02X} has truncated length"
            )));
        }
        let seg_len = u16::from_be_bytes([header[i], header[i + 1]]) as usize;
        if seg_len < 2 || i + seg_len > header.len() {
            return Err(TiffParseError::Structure(format!(
                "NDPI JPEG marker FF{marker:02X} has invalid length {seg_len}"
            )));
        }
        let payload = &header[i + 2..i + seg_len];
        if is_sof_marker(marker) {
            if payload.len() < 6 {
                return Err(TiffParseError::Structure(
                    "NDPI JPEG SOF segment too short".into(),
                ));
            }
            let component_count = payload[5] as usize;
            let components = &payload[6..];
            if components.len() < component_count * 3 {
                return Err(TiffParseError::Structure(
                    "NDPI JPEG SOF component table too short".into(),
                ));
            }
            let mut max_h = 1u8;
            let mut max_v = 1u8;
            for component in components.chunks_exact(3).take(component_count) {
                let sampling = component[1];
                let h = sampling >> 4;
                let v = sampling & 0x0F;
                if h == 0 || v == 0 {
                    return Err(TiffParseError::Structure(format!(
                        "NDPI JPEG invalid sampling {h}x{v}"
                    )));
                }
                max_h = max_h.max(h);
                max_v = max_v.max(v);
            }
            mcu_w = Some(u32::from(max_h) * 8);
            mcu_h = Some(u32::from(max_v) * 8);
        } else if marker == 0xDD {
            if payload.len() < 2 {
                return Err(TiffParseError::Structure(
                    "NDPI JPEG DRI segment too short".into(),
                ));
            }
            restart_interval = u16::from_be_bytes([payload[0], payload[1]]);
        }
        i += seg_len;
    }

    Err(TiffParseError::Structure(
        "NDPI JPEG header missing SOS marker".into(),
    ))
}

fn jpeg_header_prefix(header: &[u8]) -> Result<&[u8], TiffParseError> {
    let mut i = 0usize;
    while i + 1 < header.len() {
        if header[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = header[i + 1];
        if marker == 0xD8 || marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if i + 3 >= header.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([header[i + 2], header[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > header.len() {
            return Err(TiffParseError::Structure(format!(
                "NDPI JPEG marker FF{marker:02X} has invalid length {seg_len}"
            )));
        }
        if marker == 0xDA {
            return Ok(&header[..i + 2 + seg_len]);
        }
        i += 2 + seg_len;
    }
    Err(TiffParseError::Structure(
        "NDPI JPEG header missing SOS marker".into(),
    ))
}

// ── NdpiInterpreter ───────────────────────────────────────────────

pub(crate) struct NdpiInterpreter;

/// Intermediate representation of a classified NDPI IFD.
struct ClassifiedIfd {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    source_lens: f64,
    focal_plane: i64,
    strip_offset: u64,
    strip_byte_count: u64,
}

impl TiffLayoutInterpreter for NdpiInterpreter {
    fn detect(&self, container: &TiffContainer) -> bool {
        container.is_ndpi()
    }

    fn vendor_name(&self) -> &'static str {
        "hamamatsu-ndpi"
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let mut pyramid_ifds: Vec<ClassifiedIfd> = Vec::new();
        let mut associated_images: HashMap<String, AssociatedImage> = HashMap::new();
        let mut associated_sources: HashMap<String, TileSource> = HashMap::new();

        // Phase 1: Classify each top-level IFD
        for &ifd_id in container.top_ifds() {
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

            let source_lens = container.get_f64(ifd_id, NDPI_SOURCELENS).unwrap_or(0.0);

            if source_lens > 0.0 {
                // Pyramid level
                let focal_plane = container
                    .get_f64(ifd_id, NDPI_FOCAL_PLANE)
                    .map(|v| v as i64)
                    .unwrap_or(0);

                let strip_offset = container.get_u64(ifd_id, tags::STRIP_OFFSETS).unwrap_or(0);
                let strip_byte_count = container
                    .get_u64(ifd_id, tags::STRIP_BYTE_COUNTS)
                    .unwrap_or(0);

                if strip_offset == 0 || strip_byte_count == 0 {
                    continue;
                }

                pyramid_ifds.push(ClassifiedIfd {
                    ifd_id,
                    width,
                    height,
                    source_lens,
                    focal_plane,
                    strip_offset,
                    strip_byte_count,
                });
            } else if (source_lens as i64) == -1 {
                let name = "macro";
                let strip_offsets = match container.get_u64_array(ifd_id, tags::STRIP_OFFSETS) {
                    Ok(values) => values.to_vec(),
                    Err(_) => continue,
                };
                let strip_byte_counts =
                    match container.get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS) {
                        Ok(values) => values.to_vec(),
                        Err(_) => continue,
                    };
                if strip_offsets.is_empty() || strip_offsets.len() != strip_byte_counts.len() {
                    continue;
                }

                let compression =
                    compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1));
                let jpeg_tables = if compression == Compression::Jpeg {
                    container
                        .get_bytes(ifd_id, tags::JPEG_TABLES)
                        .ok()
                        .map(|bytes| bytes.to_vec())
                } else {
                    None
                };
                let channels = container
                    .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
                    .unwrap_or(3)
                    .clamp(1, u32::from(u16::MAX)) as u16;

                associated_images.insert(
                    name.to_string(),
                    AssociatedImage {
                        dimensions: (
                            u32::try_from(width).unwrap_or(u32::MAX),
                            u32::try_from(height).unwrap_or(u32::MAX),
                        ),
                        sample_type: SampleType::Uint8,
                        channels,
                    },
                );
                associated_sources.insert(
                    name.to_string(),
                    TileSource::Stripped {
                        ifd_id,
                        jpeg_tables,
                        compression,
                        strip_offsets,
                        strip_byte_counts,
                    },
                );
            }
            // Other SOURCELENS values (for example -2.0) remain unclassified in
            // the public dataset model to preserve compatibility associated-image
            // parity.
        }

        if pyramid_ifds.is_empty() {
            return Err(TiffParseError::Structure(
                "No pyramid levels found in NDPI".into(),
            ));
        }

        // Phase 2: Group pyramid IFDs by SOURCELENS, sort by dimensions
        // Within each SOURCELENS group, sub-group by FOCAL_PLANE for z-stack
        let (levels, tile_sources, z_count) = self.build_pyramid(container, &mut pyramid_ifds)?;

        // Phase 3: Parse properties
        let properties = self.parse_properties(container)?;

        // Phase 4: Assemble Dataset
        let property_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("No IFDs in NDPI container".into()))?;
        let lowest_resolution_ifd = pyramid_ifds
            .last()
            .map(|ifd| ifd.ifd_id)
            .ok_or_else(|| TiffParseError::Structure("No pyramid IFDs in NDPI container".into()))?;
        finish_single_scene_uint8_tiff_layout(
            container,
            lowest_resolution_ifd,
            property_ifd,
            AxesShape::new(z_count, 1, 1),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            pyramid_ifds.iter().map(|ifd| ifd.ifd_id),
        )
    }
}

impl NdpiInterpreter {
    /// Build pyramid levels from classified IFDs.
    /// Groups physical IFDs by SOURCELENS, then materializes the public NDPI
    /// pyramid as exact power-of-two downsample factors from level 0.
    /// Missing intermediate levels are synthesized from the nearest
    /// higher-resolution public level.
    ///
    /// Returns (levels, tile_sources, z_count).
    #[allow(clippy::type_complexity)]
    fn build_pyramid(
        &self,
        container: &TiffContainer,
        pyramid_ifds: &mut [ClassifiedIfd],
    ) -> Result<(Vec<Level>, HashMap<TileSourceKey, TileSource>, u32), TiffParseError> {
        // Sort by area descending (largest first = level 0)
        pyramid_ifds.sort_by(|a, b| {
            let area_a = a.width * a.height;
            let area_b = b.width * b.height;
            area_b.cmp(&area_a)
        });

        // Group by SOURCELENS value -- each distinct value is one pyramid level
        // Use a Vec of (source_lens_bits, Vec<&ClassifiedIfd>) to preserve sort order.
        let mut lens_groups: Vec<(u64, Vec<&ClassifiedIfd>)> = Vec::new();
        for ifd in pyramid_ifds.iter() {
            let bits = ifd.source_lens.to_bits();
            if let Some(group) = lens_groups.iter_mut().find(|(b, _)| *b == bits) {
                group.1.push(ifd);
            } else {
                lens_groups.push((bits, vec![ifd]));
            }
        }

        // The first group (sorted by largest area) is level 0.
        let base_dims = (pyramid_ifds[0].width, pyramid_ifds[0].height);

        // Count distinct focal planes across all IFDs
        let mut focal_planes: Vec<i64> = pyramid_ifds.iter().map(|ifd| ifd.focal_plane).collect();
        focal_planes.sort();
        focal_planes.dedup();
        let z_count = focal_planes.len().max(1) as u32;

        let mut physical_groups_by_factor: HashMap<u32, Vec<&ClassifiedIfd>> = HashMap::new();
        for (_lens_bits, group) in lens_groups {
            let representative = group[0];
            let Some(factor) =
                ndpi_power_of_two_factor(base_dims, (representative.width, representative.height))
            else {
                continue;
            };
            physical_groups_by_factor.insert(factor, group);
        }

        let mut expected_factors = Vec::new();
        let mut factor = 1u32;
        while u64::from(factor) <= base_dims.0
            && u64::from(factor) <= base_dims.1
            && base_dims.0.is_multiple_of(u64::from(factor))
            && base_dims.1.is_multiple_of(u64::from(factor))
        {
            expected_factors.push(factor);
            factor = match factor.checked_mul(2) {
                Some(next) => next,
                None => break,
            };
        }

        let mut levels = Vec::new();
        let mut tile_sources = HashMap::new();
        let mut previous_public_level_idx: Option<u32> = None;
        let mut nearest_physical_level: Option<(u32, u32)> = None;

        for expected_factor in expected_factors {
            let level_idx = levels.len() as u32;
            let width = base_dims.0 / u64::from(expected_factor);
            let height = base_dims.1 / u64::from(expected_factor);
            let downsample = expected_factor as f64;

            if let Some(group) = physical_groups_by_factor.remove(&expected_factor) {
                let representative = group[0];

                let representative_probe =
                    probe_jpeg_geometry_via_j2k(container, representative.ifd_id)?;
                let restart_interval = representative_probe.restart_interval;
                let (mcu_w, mcu_h) = (representative_probe.mcu_w, representative_probe.mcu_h);

                let (virtual_tile_width, virtual_tile_height) = if restart_interval > 0 {
                    (
                        mcu_w.checked_mul(restart_interval as u32).ok_or_else(|| {
                            TiffParseError::Structure(format!(
                                "NDPI: virtual tile width overflow (mcu_w={}, restart_interval={})",
                                mcu_w, restart_interval
                            ))
                        })?,
                        mcu_h,
                    )
                } else {
                    (
                        u32::try_from(width).unwrap_or(u32::MAX),
                        u32::try_from(height).unwrap_or(u32::MAX),
                    )
                };

                levels.push(Level {
                    dimensions: (width, height),
                    downsample,
                    tile_layout: TileLayout::WholeLevel {
                        width,
                        height,
                        virtual_tile_width,
                        virtual_tile_height,
                    },
                });

                for ifd in group {
                    let z_index = focal_planes
                        .iter()
                        .position(|&fp| fp == ifd.focal_plane)
                        .unwrap_or(0) as u32;

                    let ifd_probe = if ifd.ifd_id == representative.ifd_id {
                        representative_probe.clone()
                    } else {
                        probe_jpeg_geometry_via_j2k(container, ifd.ifd_id)?
                    };

                    let plane_ri = ifd_probe.restart_interval;
                    let (plane_mcu_w, plane_mcu_h) = (ifd_probe.mcu_w, ifd_probe.mcu_h);

                    let source = if plane_ri > 0 {
                        let plane_vtw =
                            plane_mcu_w.checked_mul(plane_ri as u32).ok_or_else(|| {
                                TiffParseError::Structure(format!(
                                    "NDPI: per-plane virtual tile width overflow (mcu_w={}, ri={})",
                                    plane_mcu_w, plane_ri
                                ))
                            })?;
                        let plane_vth = plane_mcu_h;
                        if plane_vtw == 0 || plane_vth == 0 {
                            return Err(TiffParseError::Structure(format!(
                                "NDPI: virtual tile dimensions must be > 0 (vtw={}, vth={})",
                                plane_vtw, plane_vth
                            )));
                        }
                        let width_u32 = u32::try_from(width).unwrap_or(u32::MAX);
                        let height_u32 = u32::try_from(height).unwrap_or(u32::MAX);
                        let plane_ta = width_u32.saturating_add(plane_vtw - 1) / plane_vtw;
                        let plane_td = height_u32.saturating_add(plane_vth - 1) / plane_vth;
                        TileSource::NdpiJpeg {
                            ifd_id: ifd.ifd_id,
                            jpeg_header: ifd_probe.header,
                            mcu_starts_tag: NDPI_MCU_STARTS,
                            tiles_across: plane_ta,
                            tiles_down: plane_td,
                            restart_interval: plane_ri,
                            strip_offset: ifd.strip_offset,
                            strip_byte_count: ifd.strip_byte_count,
                        }
                    } else {
                        TileSource::NdpiFullDecode {
                            ifd_id: ifd.ifd_id,
                            jpeg_header: ifd_probe.header,
                            strip_offset: ifd.strip_offset,
                            strip_byte_count: ifd.strip_byte_count,
                        }
                    };

                    tile_sources.insert(
                        TileSourceKey {
                            scene: 0usize,
                            series: 0usize,
                            level: level_idx,
                            z: z_index,
                            c: 0,
                            t: 0,
                        },
                        source,
                    );
                }
                nearest_physical_level = Some((level_idx, expected_factor));
            } else {
                let direct_physical_base =
                    nearest_physical_level.and_then(|(physical_level, physical_factor)| {
                        if expected_factor % physical_factor != 0 {
                            return None;
                        }
                        let factor = expected_factor / physical_factor;
                        (factor.is_power_of_two() && (2..=8).contains(&factor))
                            .then_some((physical_level, factor))
                    });
                let (base_level, synthetic_factor) = match direct_physical_base {
                    Some(base) => base,
                    None => (
                        previous_public_level_idx.ok_or_else(|| {
                            TiffParseError::Structure(
                                "NDPI: cannot synthesize level without a higher-resolution base"
                                    .into(),
                            )
                        })?,
                        2,
                    ),
                };
                let width_u32 = u32::try_from(width).unwrap_or(u32::MAX);
                let height_u32 = u32::try_from(height).unwrap_or(u32::MAX);
                levels.push(Level {
                    dimensions: (width, height),
                    downsample,
                    tile_layout: TileLayout::WholeLevel {
                        width,
                        height,
                        virtual_tile_width: width_u32,
                        virtual_tile_height: height_u32,
                    },
                });

                for z in 0..z_count {
                    tile_sources.insert(
                        TileSourceKey {
                            scene: 0usize,
                            series: 0usize,
                            level: level_idx,
                            z,
                            c: 0,
                            t: 0,
                        },
                        TileSource::SyntheticDownsample {
                            base_level,
                            factor: synthetic_factor,
                        },
                    );
                }
            }

            previous_public_level_idx = Some(level_idx);
        }

        Ok((levels, tile_sources, z_count))
    }

    /// Parse NDPI property map from first IFD and populate Properties.
    fn parse_properties(&self, container: &TiffContainer) -> Result<Properties, TiffParseError> {
        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "hamamatsu");

        let first_ifd = match container.top_ifds().first() {
            Some(&id) => id,
            None => return Ok(properties),
        };

        // Image description -> openslide.comment
        if let Ok(desc) = container.get_string(first_ifd, tags::IMAGE_DESCRIPTION) {
            properties.insert("openslide.comment", desc.to_string());
        }

        // SourceLens -> objective power
        if let Ok(lens) = container.get_f64(first_ifd, NDPI_SOURCELENS) {
            if lens > 0.0 {
                properties.insert("hamamatsu.SourceLens", format!("{}", lens));
                properties.insert("openslide.objective-power", format!("{}", lens));
            }
        }

        // NDPI property map: key=value\r\n pairs
        if let Ok(prop_map) = container.get_string(first_ifd, NDPI_PROPERTY_MAP) {
            for line in prop_map.split("\r\n") {
                if let Some((key, val)) = line.split_once('=') {
                    if !key.is_empty() && !val.is_empty() {
                        properties.insert(format!("hamamatsu.{}", key), val.to_string());
                    }
                }
            }
        }

        // NDPI offsets
        if let Ok(xoff) = container.get_f64(first_ifd, NDPI_XOFFSET) {
            properties.insert(
                "hamamatsu.XOffsetFromSlideCentre",
                format!("{}", xoff as i64),
            );
        }
        if let Ok(yoff) = container.get_f64(first_ifd, NDPI_YOFFSET) {
            properties.insert(
                "hamamatsu.YOffsetFromSlideCentre",
                format!("{}", yoff as i64),
            );
        }
        if let Ok(reference) = container.get_string(first_ifd, NDPI_REFERENCE) {
            properties.insert("hamamatsu.Reference", reference.to_string());
        }

        // MPP from XResolution / YResolution (NDPI stores pixels/cm, unit=3).
        let res_unit = container
            .get_u32(first_ifd, tags::RESOLUTION_UNIT)
            .unwrap_or(3); // NDPI default: centimeter
        let unit_to_microns = match res_unit {
            3 => 10_000.0, // 1 cm = 10,000 µm
            _ => 25_400.0, // 1 inch = 25,400 µm
        };
        if let Ok(x_res) = container.get_f64(first_ifd, tags::X_RESOLUTION) {
            if x_res > 0.0 {
                let mpp_x = unit_to_microns / x_res;
                properties.insert("openslide.mpp-x", format!("{mpp_x:.6}"));
            }
        }
        if let Ok(y_res) = container.get_f64(first_ifd, tags::Y_RESOLUTION) {
            if y_res > 0.0 {
                let mpp_y = unit_to_microns / y_res;
                properties.insert("openslide.mpp-y", format!("{mpp_y:.6}"));
            }
        }

        Ok(properties)
    }
}

fn ndpi_power_of_two_factor(base_dims: (u64, u64), dims: (u64, u64)) -> Option<u32> {
    let (base_w, base_h) = base_dims;
    let (width, height) = dims;
    if width == 0 || height == 0 {
        return None;
    }
    if base_w % width != 0 || base_h % height != 0 {
        return None;
    }
    let factor_w = base_w / width;
    let factor_h = base_h / height;
    if factor_w != factor_h {
        return None;
    }
    let factor = u32::try_from(factor_w).ok()?;
    if factor == 0 || !factor.is_power_of_two() {
        return None;
    }
    Some(factor)
}

#[cfg(test)]
#[path = "ndpi/tests/mod.rs"]
mod tests;
