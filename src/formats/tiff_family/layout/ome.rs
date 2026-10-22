//! OME-TIFF layout interpreter.
//!
//! Reads single-file OME-TIFF: the OME-XML in the first IFD's
//! ImageDescription maps each image's Z/C/T planes onto top-level IFDs, and
//! each plane's SubIFDs hold its reduced-resolution pyramid levels. Images
//! named `label`, `macro` or `thumbnail` become associated images; every
//! other image is a scene.

use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::TiffParseError;

use super::{DatasetLayout, TiffLayoutInterpreter};

mod layout;
mod metadata;

const VENDOR: &str = "ome-tiff";

pub(crate) struct OmeTiffInterpreter;

/// Whether a first-IFD ImageDescription carries OME-XML. The generic
/// interpreter declines exactly the files this predicate accepts.
pub(super) fn is_ome_xml_description(description: &str) -> bool {
    let lower = description.to_ascii_lowercase();
    lower.contains("<ome") || lower.contains("ome.xsd")
}

impl TiffLayoutInterpreter for OmeTiffInterpreter {
    fn vendor_name(&self) -> &'static str {
        VENDOR
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        if container.is_ndpi() {
            return false;
        }
        container.top_ifds().first().is_some_and(|&first_id| {
            container
                .get_string(first_id, tags::IMAGE_DESCRIPTION)
                .is_ok_and(is_ome_xml_description)
        })
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        layout::interpret(container)
    }
}

#[cfg(test)]
mod tests;
