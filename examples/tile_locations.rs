// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lists where a level's tiles are stored and checks the byte ranges.
//!
//! For the first `count` stored tiles that one encoded unit covers, the
//! example reads each segment with `std::fs`, rebuilds the stream with
//! `EncodedUnit::assemble`, and checks that it equals
//! `Slide::read_raw_compressed_tile`. A bare payload that needs a predictor,
//! separate sample planes, a non-native byte order or YCbCr packing to decode
//! is read raw as decoded samples instead; the example accepts that only for
//! such payloads. Uncompressed 8-bit tiles are also compared pixel for pixel
//! with `Slide::read_tile`. Comparing pixels of JPEG and other compressed
//! tiles needs a decoder outside wsi-rs; for those the example checks the
//! bytes and that `read_tile` decodes the tile.
//!
//! Each checked tile prints one JSON line with its byte ranges and codec,
//! enough to build a byte-range reference such as a kerchunk entry. Counts
//! go to stderr. Any mismatch ends the run with an error.
//!
//! ```sh
//! cargo run --release --example tile_locations -- <slide-path> [count] [level] [scene] [series]
//! ```

use std::env;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use serde_json::json;
use wsi_rs::{
    ByteOrder, ChannelOrder, Compression, EncodedTilePhotometricInterpretation, EncodedUnit,
    LevelTileLocations, PixelRect, PlanarConfiguration, PlaneSelection, Predictor, Slide,
    StoredFraming, StoredTile, TileLocation, TileRequest,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let slide = Slide::open(&args.slide_path)?;
    let locations = slide.level_tile_locations(
        args.scene,
        args.series,
        args.level,
        PlaneSelection::default(),
    )?;
    report_counts(&args, &locations);

    let mut checked = 0usize;
    let mut skipped = 0usize;
    for entry in &locations.tiles {
        if checked == args.count {
            break;
        }
        let TileLocation::Stored(stored) = &entry.location else {
            continue;
        };
        let Some(unit) = whole_tile_unit(stored, &locations) else {
            skipped += 1;
            continue;
        };
        let req = TileRequest::new(args.scene, args.series, args.level, entry.col, entry.row);
        let checks = check_tile(&slide, &req, stored, unit)?;
        println!(
            "{}",
            reference_line(entry.col, entry.row, stored, unit, checks)
        );
        checked += 1;
    }

    eprintln!(
        "checked {checked} tiles; skipped {skipped} stored tiles cut from or joined from larger units"
    );
    if checked == 0 {
        return Err("no stored tile is covered by exactly one of its own encoded images".into());
    }
    Ok(())
}

fn report_counts(args: &Args, locations: &LevelTileLocations) {
    let (mut stored, mut absent, mut not_addressable) = (0usize, 0usize, 0usize);
    for entry in &locations.tiles {
        match entry.location {
            TileLocation::Stored(_) => stored += 1,
            TileLocation::Absent => absent += 1,
            _ => not_addressable += 1,
        }
    }
    eprintln!(
        "{} level {}: {} x {} tiles of {} x {} pixels; {stored} stored, {absent} absent, \
         {not_addressable} not addressable",
        args.slide_path.display(),
        args.level,
        locations.tiles_across,
        locations.tiles_down,
        locations.tile_width,
        locations.tile_height,
    );
}

/// The unit when it is the tile's own stored image, so its stream is the
/// raw compressed tile. Edge tiles qualify when only grid padding is unused.
fn whole_tile_unit<'a>(
    stored: &'a StoredTile,
    locations: &LevelTileLocations,
) -> Option<&'a EncodedUnit> {
    let unit = stored.single_unit()?;
    let (width, height) = stored.tile_size();
    let whole = PixelRect::new(0, 0, width, height);
    let stored_size = unit.stored_size();
    let own_image = stored_size == (width, height)
        || stored_size == (locations.tile_width, locations.tile_height);
    (unit.source_region() == whole && unit.tile_region() == whole && own_image).then_some(unit)
}

/// How a tile's bytes and pixels were checked.
struct Checks {
    bytes: &'static str,
    pixels: &'static str,
}

/// Checks the assembled bytes against the raw compressed tile, and the
/// pixels against `read_tile` where no outside decoder is needed.
fn check_tile(
    slide: &Slide,
    req: &TileRequest,
    stored: &StoredTile,
    unit: &EncodedUnit,
) -> Result<Checks, Box<dyn std::error::Error>> {
    let position = format!("tile ({}, {})", req.col, req.row);
    let segments = read_segments(unit)?;
    let stream = unit.assemble(segments.iter().map(Vec::as_slice))?;
    let raw = slide.read_raw_compressed_tile(req)?;
    let codec = unit.codec();
    // The raw tile and the stored bytes hold the same samples, in the stored
    // layout or decoded by wsi-rs.
    let (bytes, samples_in, row_stride) = if stream == raw.data() {
        let stored_width = unit.stored_size().0;
        let plain = codec.compression() == Compression::None && self_describing(unit);
        ("equal", plain.then_some(raw.data()), stored_width)
    } else if unit.framing() == StoredFraming::BarePayload
        && !self_describing(unit)
        && raw.compression() == Compression::None
    {
        (
            "raw tile holds decoded samples",
            Some(raw.data()),
            raw.width(),
        )
    } else {
        return Err(format!(
            "{position}: assembled {} bytes differ from the {}-byte raw compressed tile",
            stream.len(),
            raw.data().len()
        )
        .into());
    };

    let tile = slide.read_tile(req)?;
    let samples = usize::from(codec.samples_per_pixel());
    // read_tile shows MinIsWhite samples inverted, so only their bytes compare.
    let Some(raw_samples) = samples_in
        .filter(|_| codec.bits_per_sample() == 8 && usize::from(tile.channels()) == samples)
        .filter(|_| {
            codec.photometric_interpretation() != EncodedTilePhotometricInterpretation::Monochrome1
        })
    else {
        return Ok(Checks {
            bytes,
            pixels: "decoded",
        });
    };
    let decoded = tile
        .as_u8()
        .ok_or_else(|| format!("{position}: read_tile returned non-8-bit samples"))?;
    let (width, height) = stored.tile_size();
    if (tile.width(), tile.height()) != (width, height) {
        return Err(format!(
            "{position}: read_tile is {}x{}, the location says {width}x{height}",
            tile.width(),
            tile.height()
        )
        .into());
    }
    let row_bytes = width as usize * samples;
    let stride = row_stride as usize * samples;
    for row in 0..height as usize {
        let stored_row = raw_samples.get(row * stride..row * stride + row_bytes);
        let pixels = decoded.get(row * row_bytes..(row + 1) * row_bytes);
        if stored_row.is_none() || stored_row != pixels {
            return Err(format!("{position}: stored row {row} differs from read_tile").into());
        }
    }
    Ok(Checks {
        bytes,
        pixels: "equal",
    })
}

/// Whether the stored samples are chunky, native-order, RGB-ordered, unmasked,
/// unpredicted and not subsampled, so a raw tile can pass them through
/// unchanged.
fn self_describing(unit: &EncodedUnit) -> bool {
    let codec = unit.codec();
    let native = if cfg!(target_endian = "little") {
        ByteOrder::LittleEndian
    } else {
        ByteOrder::BigEndian
    };
    let subsampled = codec.photometric_interpretation()
        == EncodedTilePhotometricInterpretation::YbrFull422
        && codec.chroma_subsampling() != Some((1, 1));
    codec.predictor() == Predictor::None
        && codec.planar_configuration() == PlanarConfiguration::Chunky
        && codec.channel_order() == ChannelOrder::Rgb
        && codec.significant_bits().is_none()
        && (codec.bits_per_sample() <= 8 || codec.byte_order() == Some(native))
        && !subsampled
}

fn read_segments(unit: &EncodedUnit) -> std::io::Result<Vec<Vec<u8>>> {
    let mut file = File::open(unit.path())?;
    unit.segments()
        .iter()
        .map(|segment| {
            let length = usize::try_from(segment.length).map_err(std::io::Error::other)?;
            let mut bytes = vec![0; length];
            file.seek(SeekFrom::Start(segment.offset))?;
            file.read_exact(&mut bytes)?;
            Ok(bytes)
        })
        .collect()
}

fn reference_line(
    col: i64,
    row: i64,
    stored: &StoredTile,
    unit: &EncodedUnit,
    checks: Checks,
) -> serde_json::Value {
    let codec = unit.codec();
    let framing = match unit.framing() {
        StoredFraming::Standalone => "standalone",
        StoredFraming::JpegTables => "jpeg_tables",
        StoredFraming::BarePayload => "bare_payload",
        StoredFraming::Prefixed => "prefixed",
        _ => "unknown",
    };
    let segments: Vec<[u64; 2]> = unit
        .segments()
        .iter()
        .map(|segment| [segment.offset, segment.length])
        .collect();
    json!({
        "col": col,
        "row": row,
        "path": unit.path().display().to_string(),
        "segments": segments,
        "framing": framing,
        "prefix_bytes": unit.prefix().len(),
        "suffix_bytes": unit.suffix().len(),
        "jpeg_tables_bytes": codec.jpeg_tables().map(<[u8]>::len),
        "compression": format!("{:?}", codec.compression()),
        "bits_per_sample": codec.bits_per_sample(),
        "samples_per_pixel": codec.samples_per_pixel(),
        "photometric": format!("{:?}", codec.photometric_interpretation()),
        "stored_size": unit.stored_size(),
        "tile_size": stored.tile_size(),
        "bytes": checks.bytes,
        "pixels": checks.pixels,
    })
}

struct Args {
    slide_path: PathBuf,
    count: usize,
    scene: usize,
    series: usize,
    level: u32,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = env::args_os().skip(1);
        let slide_path = args.next().map(PathBuf::from).ok_or_else(usage)?;
        let count = parse_optional(args.next(), 8, "count")?;
        let level = parse_optional(args.next(), 0, "level")?;
        let scene = parse_optional(args.next(), 0, "scene")?;
        let series = parse_optional(args.next(), 0, "series")?;
        if count == 0 {
            return Err("count must be greater than zero".to_string());
        }
        Ok(Self {
            slide_path,
            count,
            scene,
            series,
            level,
        })
    }
}

fn parse_optional<T>(raw: Option<std::ffi::OsString>, default: T, name: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    match raw {
        Some(raw) => raw
            .to_str()
            .ok_or_else(|| format!("{name} must be valid UTF-8"))?
            .parse()
            .map_err(|_| format!("{name} has an invalid value")),
        None => Ok(default),
    }
}

fn usage() -> String {
    "usage: cargo run --release --example tile_locations -- <slide-path> [count] [level] [scene] [series]"
        .to_string()
}
