use std::io::{self, Read};
use std::path::Path;

pub(crate) const MAX_COMPRESSED_INPUT_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_DECODED_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn checked_product_to_usize(
    factors: &[u64],
    max: u64,
    label: &str,
) -> Result<usize, String> {
    let value = factors
        .iter()
        .try_fold(1_u64, |product, factor| product.checked_mul(*factor));
    let Some(value) = value else {
        return Err(format!("{label} length overflow"));
    };
    if value > max {
        return Err(format!("{label} exceeds {max} byte safety limit"));
    }
    usize::try_from(value).map_err(|_| format!("{label} is not addressable on this platform"))
}

pub(crate) fn read_to_end_bounded(reader: impl Read, max: u64, label: &str) -> io::Result<Vec<u8>> {
    let allocation = usize::try_from(max.min(1024 * 1024)).unwrap_or(0);
    let mut output = Vec::with_capacity(allocation);
    let mut limited = reader.take(max.saturating_add(1));
    limited.read_to_end(&mut output)?;
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} exceeds {max} byte safety limit"),
        ));
    }
    Ok(output)
}

pub(crate) fn read_file_bounded(path: &Path, max: u64, label: &str) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    read_to_end_bounded(file, max, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_product_rejects_overflow_and_limit() {
        assert!(checked_product_to_usize(&[u64::MAX, 2], u64::MAX, "image").is_err());
        assert!(checked_product_to_usize(&[8, 8, 3], 100, "image").is_err());
        assert_eq!(
            checked_product_to_usize(&[8, 8, 3], 192, "image").unwrap(),
            192
        );
    }

    #[test]
    fn bounded_read_rejects_one_byte_over_limit() {
        assert_eq!(
            read_to_end_bounded(&b"1234"[..], 4, "input").unwrap(),
            b"1234"
        );
        assert!(read_to_end_bounded(&b"12345"[..], 4, "input").is_err());
    }

    #[test]
    fn bounded_file_read_uses_the_open_handle_and_rejects_oversize_input() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("input.bin");
        std::fs::write(&path, b"12345").expect("write bounded-read fixture");

        assert_eq!(read_file_bounded(&path, 5, "input").unwrap(), b"12345");
        assert_eq!(
            read_file_bounded(&path, 4, "input")
                .expect_err("one byte over the limit must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
