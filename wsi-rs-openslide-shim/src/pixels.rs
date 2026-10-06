use wsi_rs::{ColorSpace, CpuTile, CpuTileLayout, WsiError};

pub(crate) fn tile_to_premultiplied_argb(tile: CpuTile) -> Result<Vec<u32>, WsiError> {
    let pixel_count = tile_pixel_count(&tile)?;
    let mut argb = vec![0; pixel_count];
    tile_to_premultiplied_argb_into(tile, &mut argb)?;
    Ok(argb)
}

pub(crate) fn tile_to_premultiplied_argb_into(
    tile: CpuTile,
    argb: &mut [u32],
) -> Result<(), WsiError> {
    let pixel_count = tile_pixel_count(&tile)?;
    if argb.len() != pixel_count {
        return Err(WsiError::DisplayConversion(format!(
            "premultiplied ARGB destination has {} pixels, expected {pixel_count}",
            argb.len()
        )));
    }

    if let Some(bytes) = tile.as_u8() {
        match (tile.color_space(), tile.channels(), tile.layout()) {
            (ColorSpace::Rgb, 3, CpuTileLayout::Interleaved) => {
                for (dest, &[r, g, b]) in argb.iter_mut().zip(bytes.as_chunks::<3>().0) {
                    *dest = opaque_argb(r, g, b);
                }
                return Ok(());
            }
            (ColorSpace::Rgba, 4, CpuTileLayout::Interleaved) => {
                convert_rgba_to_premultiplied_argb(bytes, argb);
                return Ok(());
            }
            (ColorSpace::Grayscale, 1, _) => {
                for (dest, &value) in argb.iter_mut().zip(bytes) {
                    *dest = opaque_argb(value, value, value);
                }
                return Ok(());
            }
            _ => {}
        }
    }

    let rgba = tile.into_rgba()?;
    convert_rgba_to_premultiplied_argb(rgba.as_raw(), argb);
    Ok(())
}

fn tile_pixel_count(tile: &CpuTile) -> Result<usize, WsiError> {
    (tile.width() as usize)
        .checked_mul(tile.height() as usize)
        .ok_or_else(|| {
            WsiError::DisplayConversion(format!(
                "premultiplied ARGB dimensions overflow: {}x{}",
                tile.width(),
                tile.height()
            ))
        })
}

fn opaque_argb(r: u8, g: u8, b: u8) -> u32 {
    0xff00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

fn convert_rgba_to_premultiplied_argb(rgba: &[u8], argb: &mut [u32]) {
    for (dest, pixel) in argb.iter_mut().zip(rgba.as_chunks::<4>().0) {
        let a = pixel[3];
        let premultiply = |channel: u8| -> u8 {
            ((u16::from(channel) * u16::from(a) + 127) / 255).min(255) as u8
        };
        let r = premultiply(pixel[0]);
        let g = premultiply(pixel[1]);
        let b = premultiply(pixel[2]);
        *dest = (u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    }
}

#[cfg(test)]
mod tests;
