use crate::decode::jp2k::Jp2kColorSpace;
use crate::decode::jp2k_codestream::Jp2kCodestreamInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedInterleavedImage {
    pub width: usize,
    pub height: usize,
    pub colorspace: Jp2kColorSpace,
    pub pixels: Vec<u8>,
}

pub(crate) fn effective_output_colorspace(
    header: &Jp2kCodestreamInfo,
    requested_colorspace: Jp2kColorSpace,
) -> Jp2kColorSpace {
    if header.multiple_component_transform {
        Jp2kColorSpace::Rgb
    } else {
        requested_colorspace
    }
}

#[cfg(test)]
mod tests;
