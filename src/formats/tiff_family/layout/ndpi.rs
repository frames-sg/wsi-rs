//! NDPI layout interpreter.
//!
//! Classifies IFDs from an NDPI TiffContainer into pyramid levels,
//! associated images, and z-stack planes. Produces a DatasetLayout
//! with TileSource descriptors for each plane.

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::core::types::*;
use crate::decode::jpeg::frame::jpeg_sof_dimensions;
use crate::decode::jpeg::is_sof_marker;
use crate::formats::tiff_family::container::tags;
use crate::formats::tiff_family::container::TiffContainer;
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;
use j2k_jpeg::{Decoder as J2kJpegDecoder, JpegError};

use super::common::{
    add_tiff_resolution_properties, associated_channels, compression_from_tag, level_area,
    ResolutionStyle, SingleSceneTiffLayout,
};
use super::{DatasetLayout, TiffLayoutInterpreter, TileSource, TileSourceKey};

const JPEG_HEADER_PROBE_BYTES: u64 = 4096;

#[derive(Clone)]
struct JpegGeometryProbe {
    header: Vec<u8>,
    dimensions: (u32, u32),
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
            dimensions: info.dimensions,
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

/// Geometry of a JPEG header j2k-jpeg does not inspect, such as one stating
/// the zero frame size NDPI writes for levels past the JPEG size limit.
/// j2k-jpeg still walks its marker segments and parses its restart interval;
/// the frame header's size and sampling factors are read as stored.
fn probe_jpeg_geometry_bytes_lenient(header: &[u8]) -> Result<JpegGeometryProbe, TiffParseError> {
    let structure = |message: String| TiffParseError::Structure(message);
    if !header.starts_with(&[0xFF, 0xD8]) {
        return Err(structure("NDPI JPEG header missing SOI".into()));
    }

    let mut restart_interval = 0;
    let mut frame = None;
    for segment in j2k_jpeg::iter_segments(header) {
        let segment = segment.map_err(|err| match err {
            JpegError::InvalidMarker { offset, .. } => {
                structure(format!("NDPI JPEG marker expected at byte {offset}"))
            }
            err => structure(format!("NDPI JPEG header is malformed: {err}")),
        })?;
        match segment.marker {
            0xD9 => break,
            0xDA => {
                let (dimensions, mcu_w, mcu_h) =
                    frame.ok_or_else(|| structure("NDPI JPEG header missing SOF marker".into()))?;
                return Ok(JpegGeometryProbe {
                    header: header[..segment.payload_offset + segment.payload.len()].to_vec(),
                    dimensions,
                    restart_interval,
                    mcu_w,
                    mcu_h,
                });
            }
            0xDD => {
                restart_interval = j2k_jpeg::parse_dri(segment.payload)
                    .map_err(|err| structure(format!("NDPI JPEG DRI segment is malformed: {err}")))?
                    .unwrap_or(0);
            }
            marker if is_sof_marker(marker) => frame = Some(sof_geometry(segment.payload)?),
            _ => {}
        }
    }

    Err(structure("NDPI JPEG header missing SOS marker".into()))
}

/// The (width, height), zero sizes included, and the MCU width and height a
/// SOF payload states.
fn sof_geometry(payload: &[u8]) -> Result<((u32, u32), u32, u32), TiffParseError> {
    let structure = |message: String| TiffParseError::Structure(message);
    let (Some(dimensions), Some(&component_count)) = (jpeg_sof_dimensions(payload), payload.get(5))
    else {
        return Err(structure("NDPI JPEG SOF segment too short".into()));
    };
    let components = payload[6..]
        .get(..usize::from(component_count) * 3)
        .ok_or_else(|| structure("NDPI JPEG SOF component table too short".into()))?;
    let (mut max_h, mut max_v) = (1u8, 1u8);
    for component in components.as_chunks::<3>().0 {
        let (h, v) = (component[1] >> 4, component[1] & 0x0F);
        if h == 0 || v == 0 {
            return Err(structure(format!("NDPI JPEG invalid sampling {h}x{v}")));
        }
        max_h = max_h.max(h);
        max_v = max_v.max(v);
    }
    Ok((dimensions, u32::from(max_h) * 8, u32::from(max_v) * 8))
}

/// The bytes of `header` through its first SOS header, as j2k-jpeg walks them.
fn jpeg_header_prefix(header: &[u8]) -> Result<&[u8], TiffParseError> {
    j2k_jpeg::iter_segments(header)
        .map_while(Result::ok)
        .find(|segment| segment.marker == 0xDA)
        .map(|sos| &header[..sos.payload_offset + sos.payload.len()])
        .ok_or_else(|| TiffParseError::Structure("NDPI JPEG header missing SOS marker".into()))
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
    jpeg_probe: JpegGeometryProbe,
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

            let source_lens = container
                .get_f64(ifd_id, tags::NDPI_SOURCELENS)
                .unwrap_or(0.0);

            if source_lens > 0.0 {
                // Pyramid level
                let focal_plane = container
                    .get_f64(ifd_id, tags::NDPI_FOCAL_PLANE)
                    .map(|v| v as i64)
                    .unwrap_or(0);

                let strip_offset = container.get_u64(ifd_id, tags::STRIP_OFFSETS).unwrap_or(0);
                let strip_byte_count = container
                    .get_u64(ifd_id, tags::STRIP_BYTE_COUNTS)
                    .unwrap_or(0);

                if strip_offset == 0 || strip_byte_count == 0 {
                    continue;
                }
                let jpeg_probe = probe_jpeg_geometry_via_j2k(container, ifd_id)?;

                pyramid_ifds.push(ClassifiedIfd {
                    ifd_id,
                    width,
                    height,
                    source_lens,
                    focal_plane,
                    strip_offset,
                    strip_byte_count,
                    jpeg_probe,
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
                let channels = associated_channels(container, ifd_id);

                associated_images.insert(
                    name.to_string(),
                    AssociatedImage {
                        dimensions: (
                            u32::try_from(width).unwrap_or(u32::MAX),
                            u32::try_from(height).unwrap_or(u32::MAX),
                        ),
                        sample_type: SampleType::Uint8,
                        channels,
                        icc_profile: Vec::new(),
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
        let (levels, tile_sources, z_count) = self.build_pyramid(&mut pyramid_ifds)?;

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
        SingleSceneTiffLayout {
            lowest_resolution_ifd,
            property_ifd,
            identity_extra_strings: &[],
            axes: AxesShape::new(z_count, 1, 1),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            source_icc_ifds: pyramid_ifds.iter().map(|ifd| ifd.ifd_id),
        }
        .finish(container)
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
        pyramid_ifds: &mut [ClassifiedIfd],
    ) -> Result<(Vec<Level>, HashMap<TileSourceKey, TileSource>, u32), TiffParseError> {
        // Sort by area descending (largest first = level 0)
        pyramid_ifds.sort_by_key(|ifd| Reverse(level_area(ifd.width, ifd.height)));

        let tiff_base_dims = (pyramid_ifds[0].width, pyramid_ifds[0].height);
        for ifd in pyramid_ifds.iter_mut().skip(1) {
            (ifd.width, ifd.height) = reconcile_ndpi_dimensions(
                tiff_base_dims,
                (ifd.width, ifd.height),
                &ifd.jpeg_probe,
            )?;
        }
        pyramid_ifds.sort_by_key(|ifd| Reverse(level_area(ifd.width, ifd.height)));

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

                let representative_probe = representative.jpeg_probe.clone();
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
                        ifd.jpeg_probe.clone()
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
                            mcu_starts_tag: tags::NDPI_MCU_STARTS,
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
                        // Generated levels are read in bounded virtual tiles;
                        // region composition must not request the entire level.
                        virtual_tile_width: width_u32.min(256),
                        virtual_tile_height: height_u32.min(256),
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
        if let Ok(lens) = container.get_f64(first_ifd, tags::NDPI_SOURCELENS) {
            if lens > 0.0 {
                properties.insert("hamamatsu.SourceLens", format!("{}", lens));
                properties.insert("openslide.objective-power", format!("{}", lens));
            }
        }

        // NDPI property map: key=value\r\n pairs
        if let Ok(prop_map) = container.get_string(first_ifd, tags::NDPI_PROPERTY_MAP) {
            for line in prop_map.split("\r\n") {
                if let Some((key, val)) = line.split_once('=') {
                    if !key.is_empty() && !val.is_empty() {
                        properties.insert(format!("hamamatsu.{}", key), val.to_string());
                    }
                }
            }
        }

        // NDPI offsets
        if let Ok(xoff) = container.get_f64(first_ifd, tags::NDPI_XOFFSET) {
            properties.insert(
                "hamamatsu.XOffsetFromSlideCentre",
                format!("{}", xoff as i64),
            );
        }
        if let Ok(yoff) = container.get_f64(first_ifd, tags::NDPI_YOFFSET) {
            properties.insert(
                "hamamatsu.YOffsetFromSlideCentre",
                format!("{}", yoff as i64),
            );
        }
        if let Ok(reference) = container.get_string(first_ifd, tags::NDPI_REFERENCE) {
            properties.insert("hamamatsu.Reference", reference.to_string());
        }

        // NDPI stores pixels per centimeter and reports MPP to six decimals.
        add_tiff_resolution_properties(
            container,
            first_ifd,
            &mut properties,
            ResolutionStyle {
                default_unit: 3,
                decimals: Some(6),
            },
        );

        Ok(properties)
    }
}

fn reconcile_ndpi_dimensions(
    base_dims: (u64, u64),
    tiff_dims: (u64, u64),
    probe: &JpegGeometryProbe,
) -> Result<(u64, u64), TiffParseError> {
    let (tiff_width, tiff_height) = tiff_dims;
    if tiff_width == 0 || tiff_height == 0 {
        return Ok(tiff_dims);
    }
    let highly_downsampled = base_dims.0 / tiff_width >= 32 && base_dims.1 / tiff_height >= 32;
    let jpeg_dims = (u64::from(probe.dimensions.0), u64::from(probe.dimensions.1));
    if !highly_downsampled || jpeg_dims.0 == 0 || jpeg_dims.1 == 0 || jpeg_dims == tiff_dims {
        return Ok(tiff_dims);
    }
    if probe.restart_interval == 0 {
        return Ok(jpeg_dims);
    }
    Err(TiffParseError::Structure(format!(
        "NDPI JPEG dimension mismatch: TIFF declares {}x{}, JPEG declares {}x{}",
        tiff_width, tiff_height, jpeg_dims.0, jpeg_dims.1
    )))
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
