static inline uint ycbcr_byte_index_u32_reference(uint2 gid, uint pitch) {
    return gid.y * pitch + gid.x * 3u;
}

kernel void wsi_rs_ycbcr8_to_rgb8_u32_perf_reference(
    device const uchar *src [[buffer(0)]],
    device uchar *dst [[buffer(1)]],
    constant YcbcrToRgb8Params &params [[buffer(2)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    const uint src_idx = ycbcr_byte_index_u32_reference(gid, params.src_pitch);
    const uint dst_idx = ycbcr_byte_index_u32_reference(gid, params.dst_pitch);
    const int yy = int(src[src_idx]);
    const int cb = int(src[src_idx + 1u]) - 128;
    const int cr = int(src[src_idx + 2u]) - 128;
    dst[dst_idx] = clamp_u8_int(yy + ((1402 * cr) / 1000));
    dst[dst_idx + 1u] = clamp_u8_int(yy - ((344 * cb + 714 * cr) / 1000));
    dst[dst_idx + 2u] = clamp_u8_int(yy + ((1772 * cb) / 1000));
}
