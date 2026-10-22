//! OME-XML parsing: the subset of the OME data model needed to map image
//! planes onto IFDs and to report channel and calibration metadata.

use std::collections::BTreeMap;
use std::str::FromStr;

use crate::core::types::ChannelInfo;
use crate::decode::xml::{self, XmlNode};
use crate::formats::tiff_family::error::TiffParseError;

pub(super) struct OmeDocument {
    pub(super) uuid: Option<String>,
    pub(super) creator: Option<String>,
    /// `BinaryOnly`: the metadata lives in an external companion file.
    pub(super) binary_only: bool,
    pub(super) images: Vec<OmeImage>,
    /// `(ID, NominalMagnification)` for each Instrument objective.
    objectives: Vec<(String, String)>,
}

pub(super) struct OmeImage {
    pub(super) name: Option<String>,
    objective_id: Option<String>,
    pub(super) pixels: OmePixels,
    /// Raw attributes of `Image`, `Pixels` and each `Channel`, for properties.
    pub(super) image_attributes: BTreeMap<String, String>,
    pub(super) pixels_attributes: BTreeMap<String, String>,
    pub(super) channel_attributes: Vec<BTreeMap<String, String>>,
}

pub(super) struct OmePixels {
    pub(super) dimension_order: DimensionOrder,
    pub(super) pixel_type: String,
    pub(super) size_x: u64,
    pub(super) size_y: u64,
    pub(super) size_z: u32,
    pub(super) size_c: u32,
    pub(super) size_t: u32,
    pub(super) physical_size_x: Option<PhysicalSize>,
    pub(super) physical_size_y: Option<PhysicalSize>,
    pub(super) channels: Vec<OmeChannel>,
    pub(super) tiff_data: Vec<OmeTiffData>,
}

pub(super) struct OmeChannel {
    pub(super) samples_per_pixel: Option<u32>,
    pub(super) info: ChannelInfo,
}

pub(super) struct OmeTiffData {
    pub(super) ifd: Option<u64>,
    pub(super) plane_count: Option<u64>,
    pub(super) first_z: u32,
    pub(super) first_c: u32,
    pub(super) first_t: u32,
    pub(super) uuid: Option<String>,
    pub(super) file_name: Option<String>,
}

impl OmeTiffData {
    /// Whether this element's planes are stored in a different file.
    pub(super) fn is_external(&self, root_uuid: Option<&str>, own_file_name: Option<&str>) -> bool {
        if let (Some(uuid), Some(root_uuid)) = (self.uuid.as_deref(), root_uuid) {
            return uuid != root_uuid;
        }
        match (self.file_name.as_deref(), own_file_name) {
            (Some(file_name), Some(own)) => file_name != own,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct PhysicalSize {
    pub(super) value: f64,
    pub(super) unit: String,
}

impl PhysicalSize {
    pub(super) fn microns(&self) -> Option<f64> {
        let microns = self.value * length_unit_in_microns(&self.unit)?;
        (microns.is_finite() && microns > 0.0).then_some(microns)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Axis {
    Z,
    C,
    T,
}

/// Order of the Z, C and T axes, fastest-varying first (`XYZCT` -> Z, C, T).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DimensionOrder([Axis; 3]);

impl DimensionOrder {
    pub(super) fn parse(value: &str) -> Option<Self> {
        let axes = value
            .strip_prefix("XY")?
            .chars()
            .map(|axis| match axis {
                'Z' => Some(Axis::Z),
                'C' => Some(Axis::C),
                'T' => Some(Axis::T),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let [a, b, c]: [Axis; 3] = axes.try_into().ok()?;
        (a != b && b != c && a != c).then_some(Self([a, b, c]))
    }
}

/// Plane extents of one image after interleaved RGB samples are folded out
/// of C, so `c` counts stored planes rather than samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PlaneShape {
    pub(super) z: u32,
    pub(super) c: u32,
    pub(super) t: u32,
    pub(super) order: DimensionOrder,
}

impl PlaneShape {
    pub(super) fn plane_count(self) -> u64 {
        u64::from(self.z) * u64::from(self.c) * u64::from(self.t)
    }

    fn extent(self, axis: Axis) -> u64 {
        u64::from(match axis {
            Axis::Z => self.z,
            Axis::C => self.c,
            Axis::T => self.t,
        })
    }

    /// Linear plane index of `(z, c, t)`, or `None` when it lies outside the image.
    pub(super) fn index(self, z: u32, c: u32, t: u32) -> Option<u64> {
        if z >= self.z || c >= self.c || t >= self.t {
            return None;
        }
        let coordinate = |axis| {
            u64::from(match axis {
                Axis::Z => z,
                Axis::C => c,
                Axis::T => t,
            })
        };
        let [fast, middle, slow] = self.order.0;
        Some(
            coordinate(fast)
                + self.extent(fast) * (coordinate(middle) + self.extent(middle) * coordinate(slow)),
        )
    }

    /// `(z, c, t)` of a linear plane index below `plane_count()`.
    pub(super) fn coords(self, index: u64) -> (u32, u32, u32) {
        let [fast, middle, slow] = self.order.0;
        let rest = index / self.extent(fast);
        let values = [
            (fast, index % self.extent(fast)),
            (middle, rest % self.extent(middle)),
            (slow, rest / self.extent(middle)),
        ];
        let value_of = |axis| {
            values
                .iter()
                .find(|(candidate, _)| *candidate == axis)
                .map_or(0, |&(_, value)| u32::try_from(value).unwrap_or(u32::MAX))
        };
        (value_of(Axis::Z), value_of(Axis::C), value_of(Axis::T))
    }
}

impl OmeDocument {
    /// `NominalMagnification` of the objective used for `image`: the one its
    /// `ObjectiveSettings` references, or the only objective in the file.
    pub(super) fn objective_power(&self, image: &OmeImage) -> Option<&str> {
        match image.objective_id.as_deref() {
            Some(id) => self
                .objectives
                .iter()
                .find(|(objective_id, _)| objective_id == id)
                .map(|(_, power)| power.as_str()),
            None if self.objectives.len() == 1 => Some(self.objectives[0].1.as_str()),
            None => None,
        }
    }
}

pub(super) fn parse_ome_xml(xml_text: &str) -> Result<OmeDocument, TiffParseError> {
    let root = xml::parse_xml(xml_text)
        .map_err(|err| TiffParseError::Structure(format!("OME-TIFF: invalid OME-XML: {err}")))?;
    if local_name(&root.tag) != "OME" {
        return Err(TiffParseError::Structure(format!(
            "OME-TIFF: root element is <{}>, expected <OME>",
            root.tag
        )));
    }

    let objectives = children(&root, "Instrument")
        .flat_map(|instrument| children(instrument, "Objective"))
        .filter_map(|objective| {
            Some((
                objective.attr("ID")?.to_owned(),
                objective.attr("NominalMagnification")?.to_owned(),
            ))
        })
        .collect();
    let images = children(&root, "Image")
        .enumerate()
        .map(|(index, image)| parse_image(index, image))
        .collect::<Result<Vec<_>, _>>()?;
    if images.is_empty() {
        return Err(TiffParseError::Structure(
            "OME-TIFF: OME-XML declares no Image elements".into(),
        ));
    }

    Ok(OmeDocument {
        uuid: root.attr("UUID").map(ToOwned::to_owned),
        creator: root.attr("Creator").map(ToOwned::to_owned),
        binary_only: child(&root, "BinaryOnly").is_some(),
        images,
        objectives,
    })
}

fn parse_image(index: usize, image: &XmlNode) -> Result<OmeImage, TiffParseError> {
    let context = format!("OME-TIFF Image {index}");
    let pixels = child(image, "Pixels")
        .ok_or_else(|| TiffParseError::Structure(format!("{context} has no Pixels element")))?;

    let order_text = required_attr::<String>(pixels, "DimensionOrder", &context)?;
    let dimension_order = DimensionOrder::parse(&order_text).ok_or_else(|| {
        TiffParseError::Structure(format!(
            "{context} has invalid DimensionOrder '{order_text}'"
        ))
    })?;
    let size_x = required_attr::<u64>(pixels, "SizeX", &context)?;
    let size_y = required_attr::<u64>(pixels, "SizeY", &context)?;
    let size_z = required_attr::<u32>(pixels, "SizeZ", &context)?;
    let size_c = required_attr::<u32>(pixels, "SizeC", &context)?;
    let size_t = required_attr::<u32>(pixels, "SizeT", &context)?;
    if [size_x, size_y].contains(&0) || [size_z, size_c, size_t].contains(&0) {
        return Err(TiffParseError::Structure(format!(
            "{context} has a zero-sized dimension"
        )));
    }

    let channel_nodes = children(pixels, "Channel").collect::<Vec<_>>();
    let channels = channel_nodes
        .iter()
        .map(|channel| parse_channel(channel, &context))
        .collect::<Result<Vec<_>, _>>()?;
    let tiff_data = children(pixels, "TiffData")
        .map(|tiff_data| parse_tiff_data(tiff_data, &context))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OmeImage {
        name: image.attr("Name").map(ToOwned::to_owned),
        objective_id: child(image, "ObjectiveSettings")
            .and_then(|settings| settings.attr("ID"))
            .map(ToOwned::to_owned),
        pixels: OmePixels {
            dimension_order,
            pixel_type: required_attr::<String>(pixels, "Type", &context)?,
            size_x,
            size_y,
            size_z,
            size_c,
            size_t,
            physical_size_x: physical_size(pixels, "PhysicalSizeX", &context)?,
            physical_size_y: physical_size(pixels, "PhysicalSizeY", &context)?,
            channels,
            tiff_data,
        },
        image_attributes: image.attributes.clone(),
        pixels_attributes: pixels.attributes.clone(),
        channel_attributes: channel_nodes
            .iter()
            .map(|channel| channel.attributes.clone())
            .collect(),
    })
}

fn parse_channel(channel: &XmlNode, context: &str) -> Result<OmeChannel, TiffParseError> {
    let mut info = ChannelInfo::new();
    if let Some(name) = channel.attr("Name") {
        info = info.with_name(name);
    }
    if let Some(color) = optional_attr::<i64>(channel, "Color", context)? {
        // OME colors are signed 32-bit RGBA integers, red in the high byte.
        let rgba = (color as i32 as u32).to_be_bytes();
        info = info.with_color([rgba[0], rgba[1], rgba[2]]);
    }
    if let Some(nm) = wavelength_nm(channel, "ExcitationWavelength", context)? {
        info = info.with_excitation_nm(nm);
    }
    if let Some(nm) = wavelength_nm(channel, "EmissionWavelength", context)? {
        info = info.with_emission_nm(nm);
    }
    Ok(OmeChannel {
        samples_per_pixel: optional_attr::<u32>(channel, "SamplesPerPixel", context)?,
        info,
    })
}

fn parse_tiff_data(tiff_data: &XmlNode, context: &str) -> Result<OmeTiffData, TiffParseError> {
    let uuid = child(tiff_data, "UUID");
    Ok(OmeTiffData {
        ifd: optional_attr(tiff_data, "IFD", context)?,
        plane_count: optional_attr(tiff_data, "PlaneCount", context)?,
        first_z: optional_attr(tiff_data, "FirstZ", context)?.unwrap_or(0),
        first_c: optional_attr(tiff_data, "FirstC", context)?.unwrap_or(0),
        first_t: optional_attr(tiff_data, "FirstT", context)?.unwrap_or(0),
        uuid: uuid
            .and_then(|uuid| uuid.text.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned),
        file_name: uuid
            .and_then(|uuid| uuid.attr("FileName"))
            .map(ToOwned::to_owned),
    })
}

fn physical_size(
    pixels: &XmlNode,
    name: &str,
    context: &str,
) -> Result<Option<PhysicalSize>, TiffParseError> {
    Ok(
        optional_attr::<f64>(pixels, name, context)?.map(|value| PhysicalSize {
            value,
            unit: pixels
                .attr(&format!("{name}Unit"))
                .unwrap_or("µm")
                .to_owned(),
        }),
    )
}

fn wavelength_nm(
    channel: &XmlNode,
    name: &str,
    context: &str,
) -> Result<Option<f64>, TiffParseError> {
    let Some(value) = optional_attr::<f64>(channel, name, context)? else {
        return Ok(None);
    };
    let unit = channel.attr(&format!("{name}Unit")).unwrap_or("nm");
    Ok(length_unit_in_microns(unit)
        .map(|microns| value * microns * 1_000.0)
        .filter(|nm| nm.is_finite() && *nm > 0.0))
}

/// Microns per unit for the OME `UnitsLength` values used by microscopy.
fn length_unit_in_microns(unit: &str) -> Option<f64> {
    Some(match unit {
        "m" => 1e6,
        "cm" => 1e4,
        "mm" => 1e3,
        // U+00B5 MICRO SIGN (the schema's spelling) and U+03BC GREEK SMALL LETTER MU.
        "µm" | "μm" | "um" => 1.0,
        "nm" => 1e-3,
        "pm" => 1e-6,
        "Å" => 1e-4,
        _ => return None,
    })
}

fn local_name(tag: &str) -> &str {
    tag.rsplit(':').next().unwrap_or(tag)
}

fn children<'a>(node: &'a XmlNode, name: &'a str) -> impl Iterator<Item = &'a XmlNode> {
    node.children
        .iter()
        .filter(move |child| local_name(&child.tag) == name)
}

fn child<'a>(node: &'a XmlNode, name: &'a str) -> Option<&'a XmlNode> {
    children(node, name).next()
}

fn optional_attr<T: FromStr>(
    node: &XmlNode,
    name: &str,
    context: &str,
) -> Result<Option<T>, TiffParseError> {
    node.attr(name)
        .map(|value| {
            value.trim().parse::<T>().map_err(|_| {
                TiffParseError::Structure(format!(
                    "{context}: {} attribute {name}='{value}' is invalid",
                    local_name(&node.tag)
                ))
            })
        })
        .transpose()
}

fn required_attr<T: FromStr>(
    node: &XmlNode,
    name: &str,
    context: &str,
) -> Result<T, TiffParseError> {
    optional_attr(node, name, context)?.ok_or_else(|| {
        TiffParseError::Structure(format!(
            "{context}: {} is missing required attribute {name}",
            local_name(&node.tag)
        ))
    })
}
