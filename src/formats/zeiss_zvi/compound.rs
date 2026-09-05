use super::*;

pub(super) fn looks_like_zvi(compound: &mut CompoundFile<File>) -> bool {
    compound.is_stream("/Image/Tags/Contents")
        && compound
            .walk()
            .any(|entry| entry.is_stream() && item_contents_index(&entry_path(&entry)).is_some())
}

pub(super) fn compound_stream_paths_with_budget(
    compound: &CompoundFile<File>,
    budget: &OpenBudget,
) -> Result<Vec<String>, WsiError> {
    let mut paths = Vec::new();
    for entry in compound.walk().filter(|entry| entry.is_stream()) {
        if paths.len() == MAX_ZVI_STREAMS {
            return Err(WsiError::DisplayConversion(format!(
                "ZVI compound file exceeds the {MAX_ZVI_STREAMS}-stream safety limit"
            )));
        }
        let path = entry_path(&entry);
        let retained_bytes = u64::try_from(std::mem::size_of::<String>())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(path.len()).unwrap_or(u64::MAX));
        budget.retain_index(retained_bytes)?;
        paths.try_reserve(1).map_err(|_| WsiError::ResourceLimit {
            resource: "ZVI compound stream index",
            requested: retained_bytes,
            limit: budget.limits().tile_index_bytes(),
        })?;
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

fn entry_path(entry: &cfb::Entry) -> String {
    entry.path().to_string_lossy().replace('\\', "/")
}

pub(super) fn item_contents_index(path: &str) -> Option<i32> {
    let rest = path.strip_prefix("/Image/Item(")?;
    let (index, suffix) = rest.split_once(')')?;
    (suffix == "/Contents")
        .then(|| index.parse::<i32>().ok())
        .flatten()
}

pub(super) fn read_stream_prefix_with_budget(
    compound: &mut CompoundFile<File>,
    path: &str,
    structural_limit: usize,
    label: &'static str,
    budget: &OpenBudget,
) -> Result<Vec<u8>, WsiError> {
    let mut stream = compound.open_stream(path)?;
    let stream_len = stream.seek(std::io::SeekFrom::End(0))?;
    stream.seek(std::io::SeekFrom::Start(0))?;
    let requested = stream_len.min(u64::try_from(structural_limit).unwrap_or(u64::MAX));
    budget.check_metadata_value(requested)?;
    let requested_usize = usize::try_from(requested).map_err(|_| WsiError::ResourceLimit {
        resource: label,
        requested,
        limit: budget.limits().metadata_value_bytes(),
    })?;
    let mut data = Vec::new();
    data.try_reserve_exact(requested_usize)
        .map_err(|_| WsiError::ResourceLimit {
            resource: label,
            requested,
            limit: budget.limits().metadata_value_bytes(),
        })?;
    data.resize(requested_usize, 0);
    stream.read_exact(&mut data)?;
    Ok(data)
}

pub(super) fn read_stream_bounded_with_budget(
    compound: &mut CompoundFile<File>,
    path: &str,
    structural_limit: u64,
    label: &str,
    budget: &OpenBudget,
    retain_metadata: bool,
) -> Result<Vec<u8>, WsiError> {
    let limit = structural_limit.min(budget.limits().metadata_value_bytes());
    let mut stream = compound.open_stream(path)?;
    let declared_len = stream.seek(std::io::SeekFrom::End(0))?;
    stream.seek(std::io::SeekFrom::Start(0))?;
    if declared_len > limit {
        return Err(WsiError::ResourceLimit {
            resource: "individual metadata value",
            requested: declared_len,
            limit,
        });
    }
    if retain_metadata {
        budget.retain_metadata(declared_len)?;
    } else {
        budget.check_metadata_value(declared_len)?;
    }
    Ok(crate::core::limits::read_to_end_bounded(
        stream, limit, label,
    )?)
}

pub(super) fn read_stream_declared_bounded(
    compound: &mut CompoundFile<File>,
    path: &str,
    limit: u64,
    label: &str,
    resource: &'static str,
) -> Result<Vec<u8>, WsiError> {
    let mut stream = compound.open_stream(path)?;
    let declared_len = stream.seek(std::io::SeekFrom::End(0))?;
    stream.seek(std::io::SeekFrom::Start(0))?;
    if declared_len > limit {
        return Err(WsiError::ResourceLimit {
            resource,
            requested: declared_len,
            limit,
        });
    }
    Ok(crate::core::limits::read_to_end_bounded(
        stream, limit, label,
    )?)
}
