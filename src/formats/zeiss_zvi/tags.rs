use super::compound::read_stream_bounded_with_budget;
use super::header::ByteReader;
use super::*;

pub(super) fn read_tags_if_present(
    compound: &mut CompoundFile<File>,
    path: &str,
    budget: &OpenBudget,
) -> Result<HashMap<i32, String>, WsiError> {
    if !compound.is_stream(path) {
        return Ok(HashMap::new());
    }
    let data = read_stream_bounded_with_budget(
        compound,
        path,
        MAX_ZVI_METADATA_BYTES,
        "ZVI metadata stream",
        budget,
        true,
    )?;
    parse_zvi_tags_with_budget(&data, budget)
}

#[cfg(test)]
fn parse_zvi_tags(data: &[u8]) -> Result<HashMap<i32, String>, WsiError> {
    let budget = OpenBudget::new(crate::SlideLimits::default());
    parse_zvi_tags_with_budget(data, &budget)
}

fn parse_zvi_tags_with_budget(
    data: &[u8],
    budget: &OpenBudget,
) -> Result<HashMap<i32, String>, WsiError> {
    let mut reader = ByteReader::new(data);
    reader.skip(8)?;
    let count = reader.read_i32()?.max(0) as usize;
    if count > MAX_ZVI_TAGS {
        return Err(WsiError::DisplayConversion(format!(
            "ZVI tag count {count} exceeds the {MAX_ZVI_TAGS}-tag safety limit"
        )));
    }
    let mut tags = HashMap::new();
    let tag_index_bytes = u64::try_from(count)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(std::mem::size_of::<(i32, String)>()).unwrap_or(u64::MAX));
    budget.retain_index(tag_index_bytes)?;
    tags.try_reserve(count)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "ZVI tag index",
            requested: tag_index_bytes,
            limit: budget.limits().tile_index_bytes(),
        })?;
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
        value.parse::<u64>().ok().or_else(|| {
            value.parse::<f64>().ok().and_then(|value| {
                let rounded = value.round();
                (rounded.is_finite() && rounded >= 0.0 && rounded < u64::MAX as f64)
                    .then_some(rounded as u64)
            })
        })
    })
}

pub(super) fn tag_f64(tags: &HashMap<i32, String>, tag_id: i32) -> Option<f64> {
    tag_string(tags, tag_id)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

pub(super) fn tag_color(tags: &HashMap<i32, String>, tag_id: i32) -> Option<[u8; 3]> {
    let value = tag_string(tags, tag_id)?.parse::<u32>().ok()?;
    Some([
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ])
}

#[cfg(test)]
#[path = "tests/tags.rs"]
mod tests;
