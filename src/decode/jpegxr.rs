//! JPEG XR container-to-codec adapter. Entropy and reconstruction belong to jxr.
use jxr::{
    AlphaMode, ChannelLayout, ColorFormat, DecodeLimits, DecodeRequest, DecodedSamples, JxrView,
};

use crate::core::limits::{checked_product_to_usize, SlideLimits};
use crate::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout, SampleType, WsiError};

pub(crate) fn decode_jpegxr(
    data: &[u8],
    width: u32,
    height: u32,
    sample_type: SampleType,
    channels: u16,
    limits: SlideLimits,
) -> Result<CpuTile, WsiError> {
    let error = |source| WsiError::Codec {
        codec: "jpeg-xr",
        source: Box::new(source),
    };
    if data.len() as u64 > limits.encoded_unit_bytes() {
        return Err(WsiError::ResourceLimit {
            resource: "encoded JPEG XR tile",
            requested: data.len() as u64,
            limit: limits.encoded_unit_bytes(),
        });
    }
    let layout = match channels {
        1 => ChannelLayout::Luma,
        3 => ChannelLayout::Rgb,
        _ => {
            return Err(WsiError::UnsupportedFormat(
                "JPEG XR tiles require grayscale or RGB pixels".into(),
            ))
        }
    };
    let (format, source_format, bytes_per_sample) = match sample_type {
        SampleType::Uint8 => (
            jxr::PixelFormat::U8(layout),
            jxr::SampleFormat::Unsigned { bits: 8 },
            1,
        ),
        SampleType::Uint16 => (
            jxr::PixelFormat::U16(layout),
            jxr::SampleFormat::Unsigned { bits: 16 },
            2,
        ),
        SampleType::Float32 => (jxr::PixelFormat::F32(layout), jxr::SampleFormat::Float32, 4),
    };
    let output_bytes = checked_product_to_usize(
        &[
            u64::from(width),
            u64::from(height),
            u64::from(channels),
            bytes_per_sample,
        ],
        limits.decoded_output_bytes(),
        "JPEG XR decoded tile",
    )
    .map_err(WsiError::UnsupportedFormat)?;
    let view = JxrView::parse(data).map_err(error)?;
    let info = view.info();
    if info.dimensions() != (width, height) {
        return Err(WsiError::UnsupportedFormat(format!(
            "JPEG XR dimensions {}x{} do not match container {width}x{height}",
            info.width, info.height
        )));
    }
    let compatible_color = match channels {
        1 => info.primary.color_format == ColorFormat::Luma,
        3 => matches!(
            info.primary.color_format,
            ColorFormat::Rgb | ColorFormat::Yuv(_)
        ),
        _ => false,
    };
    if !compatible_color
        || info.primary.sample_format != source_format
        || info.alpha_mode != AlphaMode::None
    {
        return Err(WsiError::UnsupportedFormat(
            "JPEG XR sample type, color, or alpha differs from the container pixel contract".into(),
        ));
    }
    let reserved = (data.len() as u64).saturating_add((output_bytes as u64).saturating_mul(2));
    let codec_budget = limits
        .operation_transient_bytes()
        .checked_sub(reserved)
        .filter(|&n| n >= 2)
        .ok_or(WsiError::ResourceLimit {
            resource: "JPEG XR transient work",
            requested: reserved,
            limit: limits.operation_transient_bytes(),
        })?
        / 2;
    let request = DecodeRequest::new(format)
        .with_backend(jxr::BackendRequest::Cpu)
        .with_limits(DecodeLimits {
            max_width: width,
            max_height: height,
            max_pixels: u64::from(width) * u64::from(height),
            max_components: channels,
            max_compressed_bytes: limits.encoded_unit_bytes(),
            max_coefficient_bytes: codec_budget,
            max_host_allocation_bytes: codec_budget,
            ..DecodeLimits::default()
        });
    let decoded = view.decoder().decode(&request).map_err(error)?;
    decoded.validate_layout().map_err(error)?;
    let samples = match decoded.samples {
        DecodedSamples::U8(v) => CpuTileData::u8(v),
        DecodedSamples::U16(v) => CpuTileData::u16(v),
        DecodedSamples::F32(v) => CpuTileData::f32(v),
        _ => {
            return Err(WsiError::BackendContract {
                context: "JPEG XR decode",
                expected: channels as usize,
                actual: 0,
            })
        }
    };
    CpuTile::new(
        width,
        height,
        channels,
        if channels == 1 {
            ColorSpace::Grayscale
        } else {
            ColorSpace::Rgb
        },
        CpuTileLayout::Interleaved,
        samples,
    )
}

#[cfg(test)]
mod tests;
