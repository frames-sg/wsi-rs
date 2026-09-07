#include <metal_stdlib>
using namespace metal;

struct YcbcrToRgb8Params {
    uint width;
    uint height;
    uint src_pitch;
    uint dst_pitch;
};

static inline uchar clamp_u8_int(int value) {
    return uchar(clamp(value, 0, 255));
}

static inline ulong ycbcr_byte_index(uint2 gid, uint pitch) {
    return ulong(gid.y) * ulong(pitch) + ulong(gid.x) * 3ul;
}

static inline uint ycbcr_byte_index_u32(uint2 gid, uint pitch) {
    return gid.y * pitch + gid.x * 3u;
}

template <typename Index>
static inline void ycbcr_to_rgb8_pixel(
    device const uchar *src,
    device uchar *dst,
    constant int *tables,
    Index src_idx,
    Index dst_idx
) {
    const int yy = int(src[src_idx]);
    const uint cb = uint(src[src_idx + Index(1)]);
    const uint cr = uint(src[src_idx + Index(2)]);
    dst[dst_idx] = clamp_u8_int(yy + tables[cr]);
    dst[dst_idx + Index(1)] = clamp_u8_int(yy + ((tables[256u + cb] + tables[512u + cr]) >> 16));
    dst[dst_idx + Index(2)] = clamp_u8_int(yy + tables[768u + cb]);
}

kernel void wsi_rs_ycbcr8_to_rgb8_u32(
    device const uchar *src [[buffer(0)]],
    device uchar *dst [[buffer(1)]],
    constant YcbcrToRgb8Params &params [[buffer(2)]],
    constant int *tables [[buffer(3)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    ycbcr_to_rgb8_pixel(
        src,
        dst,
        tables,
        ycbcr_byte_index_u32(gid, params.src_pitch),
        ycbcr_byte_index_u32(gid, params.dst_pitch)
    );
}

kernel void wsi_rs_ycbcr8_to_rgb8(
    device const uchar *src [[buffer(0)]],
    device uchar *dst [[buffer(1)]],
    constant YcbcrToRgb8Params &params [[buffer(2)]],
    constant int *tables [[buffer(3)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    ycbcr_to_rgb8_pixel(
        src,
        dst,
        tables,
        ycbcr_byte_index(gid, params.src_pitch),
        ycbcr_byte_index(gid, params.dst_pitch)
    );
}
