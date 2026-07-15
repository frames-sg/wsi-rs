kernel void wsi_rs_ycbcr8_address_probe(
    device ulong *result [[buffer(0)]],
    constant YcbcrToRgb8Params &params [[buffer(1)]],
    constant uint2 &coordinate [[buffer(2)]]
) {
    result[0] = ycbcr_byte_index(coordinate, params.src_pitch);
    result[1] = ycbcr_byte_index(coordinate, params.dst_pitch);
}
