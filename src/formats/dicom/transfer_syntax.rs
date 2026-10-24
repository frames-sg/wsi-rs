//! The transfer syntaxes this reader decodes, classified once per image so
//! per-tile dispatch matches an enum rather than UID strings.

use std::path::Path;

use dicom_dictionary_std::uids;

use super::decode::jp2k_photometric_is_ycbcr;
use crate::core::types::{Compression, TileCodecKind};
use crate::error::WsiError;

// Retired but still decodable Huffman-coded progressive JPEG syntaxes.
pub(super) const JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.53";
pub(super) const JPEG_FULL_PROGRESSION_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.55";
pub(super) const EXPLICIT_VR_BIG_ENDIAN_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.2";
pub(super) const HTJ2K_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.203";
pub(super) const HTJ2K_LOSSLESS_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.201";
pub(super) const HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.202";

/// A supported transfer syntax, by how its Pixel Data is stored and decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DicomTransferSyntax {
    /// Uncompressed Pixel Data in the dataset's byte order.
    Native { big_endian: bool },
    /// JPEG frames of one coding process.
    Jpeg(DicomJpegProcess),
    /// JPEG 2000 or, when `ht`, HTJ2K codestreams. `lossless_only` marks the
    /// syntaxes that admit only reversible coding.
    Jp2k { ht: bool, lossless_only: bool },
    /// RLE Lossless frames.
    Rle,
}

/// The JPEG coding process a JPEG transfer syntax declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DicomJpegProcess {
    /// Baseline 8-bit DCT.
    Baseline,
    /// Extended or retired progressive DCT, which also carries 12-bit samples.
    Extended,
    /// Lossless with any predictor.
    Lossless,
    /// Lossless with first-order prediction (selection value 1) only.
    LosslessSv1,
}

impl DicomTransferSyntax {
    /// The classification of `uid`, or `None` when this reader cannot decode
    /// that transfer syntax.
    pub(super) fn from_uid(uid: &str) -> Option<Self> {
        Some(match uid {
            uids::IMPLICIT_VR_LITTLE_ENDIAN | uids::EXPLICIT_VR_LITTLE_ENDIAN => {
                Self::Native { big_endian: false }
            }
            EXPLICIT_VR_BIG_ENDIAN_TRANSFER_SYNTAX => Self::Native { big_endian: true },
            uids::JPEG_BASELINE8_BIT => Self::Jpeg(DicomJpegProcess::Baseline),
            uids::JPEG_EXTENDED12_BIT
            | JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX
            | JPEG_FULL_PROGRESSION_TRANSFER_SYNTAX => Self::Jpeg(DicomJpegProcess::Extended),
            uids::JPEG_LOSSLESS => Self::Jpeg(DicomJpegProcess::Lossless),
            uids::JPEG_LOSSLESS_SV1 => Self::Jpeg(DicomJpegProcess::LosslessSv1),
            uids::JPEG2000_LOSSLESS => Self::Jp2k {
                ht: false,
                lossless_only: true,
            },
            uids::JPEG2000 => Self::Jp2k {
                ht: false,
                lossless_only: false,
            },
            HTJ2K_LOSSLESS_TRANSFER_SYNTAX | HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX => Self::Jp2k {
                ht: true,
                lossless_only: true,
            },
            HTJ2K_TRANSFER_SYNTAX => Self::Jp2k {
                ht: true,
                lossless_only: false,
            },
            uids::RLE_LOSSLESS => Self::Rle,
            _ => return None,
        })
    }

    /// The classification of `uid`, or an invalid-slide error naming `path`
    /// when this reader cannot decode that transfer syntax.
    pub(super) fn supported(path: &Path, uid: &str) -> Result<Self, WsiError> {
        Self::from_uid(uid).ok_or_else(|| {
            WsiError::invalid_slide(path, format!("Unsupported transfer syntax {uid}"))
        })
    }

    /// Whether Pixel Data is a sequence of encapsulated frames.
    pub(super) fn is_encapsulated(self) -> bool {
        !matches!(self, Self::Native { .. })
    }

    /// Whether the transfer syntax itself is lossy; `None` for JPEG 2000 and
    /// HTJ2K syntaxes whose codestreams may be either.
    pub(super) fn is_lossy(self) -> Option<bool> {
        match self {
            Self::Jpeg(process) => Some(!process.is_lossless()),
            Self::Jp2k {
                lossless_only: false,
                ..
            } => None,
            Self::Native { .. } | Self::Jp2k { .. } | Self::Rle => Some(false),
        }
    }

    pub(super) fn tile_codec_kind(self) -> TileCodecKind {
        match self {
            Self::Jpeg(_) => TileCodecKind::Jpeg,
            Self::Jp2k { ht: true, .. } => TileCodecKind::Htj2k,
            Self::Jp2k { ht: false, .. } => TileCodecKind::Jp2k,
            Self::Native { .. } | Self::Rle => TileCodecKind::Other,
        }
    }

    /// The compression of the stored Pixel Data.
    pub(super) fn source_compression(self, photometric_interpretation: &str) -> Compression {
        match self {
            Self::Native { .. } => Compression::None,
            Self::Rle => Compression::Rle,
            Self::Jpeg(_) => Compression::Jpeg,
            Self::Jp2k { .. } if jp2k_photometric_is_ycbcr(photometric_interpretation) => {
                Compression::Jp2kYcbcr
            }
            Self::Jp2k { .. } => Compression::Jp2kRgb,
        }
    }

    /// The compression of frames handed out as raw compressed tiles; only
    /// JPEG and JPEG 2000 frames pass through.
    pub(super) fn raw_compression(self, photometric_interpretation: &str) -> Option<Compression> {
        match self {
            Self::Jpeg(_) | Self::Jp2k { .. } => {
                Some(self.source_compression(photometric_interpretation))
            }
            Self::Native { .. } | Self::Rle => None,
        }
    }
}

impl DicomJpegProcess {
    pub(super) fn is_lossless(self) -> bool {
        matches!(self, Self::Lossless | Self::LosslessSv1)
    }

    /// Whether a frame with this SOF process and SOS predictor byte matches
    /// the declared process.
    pub(super) fn accepts(self, sof: j2k_jpeg::SofKind, lossless_predictor: Option<u8>) -> bool {
        match self {
            // Scanners such as 3DHISTECH store progressive frames under the
            // baseline syntax. The decoder follows each frame's own DCT
            // process, and the precision check keeps the declared sample type.
            Self::Baseline | Self::Extended => sof != j2k_jpeg::SofKind::Lossless,
            Self::Lossless => sof == j2k_jpeg::SofKind::Lossless,
            Self::LosslessSv1 => {
                sof == j2k_jpeg::SofKind::Lossless && lossless_predictor == Some(1)
            }
        }
    }
}
