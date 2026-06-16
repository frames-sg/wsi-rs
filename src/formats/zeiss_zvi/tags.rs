use super::compound::read_stream_to_end;
use super::header::ByteReader;
use super::*;

pub(super) fn read_tags_if_present(
    compound: &mut CompoundFile<File>,
    path: &str,
) -> Result<HashMap<i32, String>, WsiError> {
    if !compound.is_stream(path) {
        return Ok(HashMap::new());
    }
    let data = read_stream_to_end(compound, path)?;
    parse_zvi_tags(&data)
}

fn parse_zvi_tags(data: &[u8]) -> Result<HashMap<i32, String>, WsiError> {
    let mut reader = ByteReader::new(data);
    reader.skip(8)?;
    let count = reader.read_i32()?.max(0) as usize;
    let mut tags = HashMap::new();
    for _ in 0..count {
        if reader.remaining() < 2 {
            break;
        }
        let value = reader
            .read_variant()?
            .trim_matches(char::from(0))
            .trim()
            .to_string();
        reader.skip(2)?;
        if reader.remaining() < 10 {
            break;
        }
        let tag_id = reader.read_i32()?;
        reader.skip(6)?;
        if tag_id != 1047 {
            tags.insert(tag_id, value);
        }
    }
    Ok(tags)
}

pub(super) fn tag_string(tags: &HashMap<i32, String>, tag_id: i32) -> Option<String> {
    tags.get(&tag_id)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn tag_u32(tags: &HashMap<i32, String>, tag_id: i32) -> Option<u64> {
    tag_string(tags, tag_id).and_then(|value| {
        value
            .parse::<u64>()
            .ok()
            .or_else(|| value.parse::<f64>().ok().map(|v| v.round() as u64))
    })
}

pub(super) fn tag_f64(tags: &HashMap<i32, String>, tag_id: i32) -> Option<f64> {
    tag_string(tags, tag_id).and_then(|value| value.parse::<f64>().ok())
}

pub(super) fn tag_color(tags: &HashMap<i32, String>, tag_id: i32) -> Option<[u8; 3]> {
    let value = tag_string(tags, tag_id)?.parse::<u32>().ok()?;
    Some([
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ])
}
