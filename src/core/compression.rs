//! Whole-buffer zlib and Zstandard helpers built on the J2K tile codecs.
//!
//! Callers pass a decoded-size limit that they have already checked against
//! the decode limits, so the output buffer is allocated once at that size.

use j2k_tilecodec::{
    DeflateCodec, DeflatePool, TileCodecError, TileDecompress, ZstdCodec, ZstdPool,
};

/// Inflates a zlib-wrapped (or raw DEFLATE) stream of at most `max_len` decoded bytes.
pub(crate) fn decode_zlib_bounded(input: &[u8], max_len: usize) -> Result<Vec<u8>, TileCodecError> {
    let mut out = vec![0; max_len];
    let written = DeflateCodec::decompress_into(&mut DeflatePool::new(), input, &mut out)?;
    out.truncate(written);
    Ok(out)
}

/// Decodes a Zstandard frame of at most `max_len` decoded bytes.
pub(crate) fn decode_zstd_bounded(input: &[u8], max_len: usize) -> Result<Vec<u8>, TileCodecError> {
    let mut out = vec![0; max_len];
    let written = ZstdCodec::decompress_into(&mut ZstdPool::new(), input, &mut out)?;
    out.truncate(written);
    Ok(out)
}

/// Compresses with Zstandard level 1, the level svcache files have always used.
pub(crate) fn encode_zstd(input: &[u8]) -> std::io::Result<Vec<u8>> {
    zstd::bulk::compress(input, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_round_trip_respects_the_decoded_limit() {
        let raw: Vec<u8> = (0..4096u32).map(|value| (value % 251) as u8).collect();
        let encoded = encode_zstd(&raw).unwrap();
        assert_eq!(decode_zstd_bounded(&encoded, raw.len()).unwrap(), raw);
        assert!(decode_zstd_bounded(&encoded, raw.len() - 1).is_err());
    }

    #[test]
    fn zlib_decode_respects_the_decoded_limit() {
        use std::io::Write;
        let raw = b"MIRAX position buffer".repeat(64);
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        let encoded = encoder.finish().unwrap();
        assert_eq!(decode_zlib_bounded(&encoded, raw.len()).unwrap(), raw);
        assert!(decode_zlib_bounded(&encoded, raw.len() - 1).is_err());
    }
}
