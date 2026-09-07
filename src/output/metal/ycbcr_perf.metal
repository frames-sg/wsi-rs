static inline uint ycbcr_byte_index_u32_reference(uint2 gid, uint pitch) {
    return gid.y * pitch + gid.x * 3u;
}

kernel void wsi_rs_ycbcr8_to_rgb8_u32_perf_reference(
    device const uchar *src [[buffer(0)]],
    device uchar *dst [[buffer(1)]],
    constant YcbcrToRgb8Params &params [[buffer(2)]],
    constant int *tables [[buffer(3)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    const uint src_idx = ycbcr_byte_index_u32_reference(gid, params.src_pitch);
    const uint dst_idx = ycbcr_byte_index_u32_reference(gid, params.dst_pitch);
    ycbcr_to_rgb8_pixel(src, dst, tables, src_idx, dst_idx);
}
