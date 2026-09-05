use super::*;

impl TiffPixelReader {
    pub(super) fn decode_jpegxr_tile(
        &self,
        ifd: IfdId,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        let invalid = || {
            WsiError::UnsupportedFormat("JPEG XR TIFF requires contiguous unsigned 8-bit grayscale/RGB, top-left orientation, and no predictor or alpha".into())
        };
        let channels = self
            .container
            .get_u32(ifd, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1);
        let photometric = self.container.get_u32(ifd, tags::PHOTOMETRIC).unwrap_or(1);
        let bits = self
            .container
            .get_u64_array(ifd, tags::BITS_PER_SAMPLE)
            .map_err(|_| invalid())?;
        let depth = bits.first().copied().ok_or_else(invalid)?;
        if !matches!((channels, photometric), (1, 1) | (3, 2))
            || !(bits.len() == 1 || bits.len() == channels as usize)
            || bits.iter().any(|&value| value != depth)
            || self
                .container
                .get_u32(ifd, tags::PLANAR_CONFIGURATION)
                .unwrap_or(1)
                != 1
            || self.container.get_u32(ifd, tags::ORIENTATION).unwrap_or(1) != 1
            || self.container.get_u32(ifd, tags::PREDICTOR).unwrap_or(1) != 1
            || self
                .container
                .get_u64_array(ifd, 338)
                .is_ok_and(|values| !values.is_empty())
        {
            return Err(invalid());
        }
        let sample_format = self
            .container
            .get_u32(ifd, tags::SAMPLE_FORMAT)
            .unwrap_or(1);
        let sample_type = match (sample_format, depth) {
            (1, 8) => SampleType::Uint8,
            _ => return Err(invalid()),
        };
        crate::decode::jpegxr::decode_jpegxr(
            data,
            width,
            height,
            sample_type,
            channels as u16,
            self.container.limits(),
        )
    }
}
