// SPDX-License-Identifier: MIT OR Apache-2.0

//! List a level's tile locations and assemble bytes from their file ranges.
//! For whole compressed tiles, also compare with `read_raw_compressed_tile`.
//!
//! cargo run --example tile_locations -- <slide-path> [count] [level] [scene] [series]
//! `count` defaults to one assembled tile; all locations are listed.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use wsi_rs::{PixelRect, PlaneSelection, Slide, StoredFraming, TileLocation, TileRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("usage: tile_locations <slide-path> [count] [level] [scene] [series]")?;
    let count = number(args.next(), 1usize)?;
    let level = number(args.next(), 0u32)?;
    let scene = number(args.next(), 0usize)?;
    let series = number(args.next(), 0usize)?;
    if count == 0 {
        return Err("count must be greater than zero".into());
    }
    let slide = Slide::open(path)?;
    let locations = slide.level_tile_locations(scene, series, level, PlaneSelection::default())?;
    let mut assembled = 0;
    for entry in &locations.tiles {
        println!("({}, {}): {:?}", entry.col, entry.row, entry.location);
        let TileLocation::Stored(stored) = &entry.location else {
            continue;
        };
        let Some(unit) = stored.single_unit() else {
            continue;
        };
        if assembled == count {
            continue;
        }
        let mut file = File::open(unit.path())?;
        let mut segments = Vec::new();
        for range in unit.segments() {
            let mut bytes = vec![0; usize::try_from(range.length)?];
            file.seek(SeekFrom::Start(range.offset))?;
            file.read_exact(&mut bytes)?;
            segments.push(bytes);
        }
        let stream = unit.assemble(segments.iter().map(Vec::as_slice))?;
        println!("assembled {} bytes", stream.len());
        assembled += 1;

        // Crops, compositions and bare payloads may be transformed by raw reads.
        let (width, height) = stored.tile_size();
        let whole = PixelRect::new(0, 0, width, height);
        if unit.framing() != StoredFraming::BarePayload
            && unit.source_region() == whole
            && unit.tile_region() == whole
            && (unit.stored_size() == stored.tile_size()
                || unit.stored_size() == (locations.tile_width, locations.tile_height))
        {
            let request = TileRequest::new(scene, series, level, entry.col, entry.row);
            assert_eq!(stream, slide.read_raw_compressed_tile(&request)?.data());
        }
    }
    Ok(())
}

fn number<T: std::str::FromStr>(
    arg: Option<std::ffi::OsString>,
    default: T,
) -> Result<T, Box<dyn std::error::Error>> {
    match arg {
        Some(arg) => arg
            .to_str()
            .ok_or("expected a UTF-8 number")?
            .parse()
            .map_err(|_| "invalid number".into()),
        None => Ok(default),
    }
}
