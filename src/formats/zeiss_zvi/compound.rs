use super::*;

pub(super) fn looks_like_zvi(compound: &mut CompoundFile<File>) -> bool {
    compound.is_stream("/Image/Tags/Contents")
        && compound
            .walk()
            .any(|entry| entry.is_stream() && item_contents_index(&entry_path(&entry)).is_some())
}

pub(super) fn compound_stream_paths(compound: &CompoundFile<File>) -> Vec<String> {
    let mut paths = compound
        .walk()
        .filter(|entry| entry.is_stream())
        .map(|entry| entry_path(&entry))
        .collect::<Vec<_>>();
    paths.sort();
    paths
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

pub(super) fn read_stream_prefix(
    compound: &mut CompoundFile<File>,
    path: &str,
    limit: usize,
) -> Result<Vec<u8>, WsiError> {
    let mut stream = compound.open_stream(path)?;
    let mut data = vec![0u8; limit];
    let count = stream.read(&mut data)?;
    data.truncate(count);
    Ok(data)
}

pub(super) fn read_stream_to_end(
    compound: &mut CompoundFile<File>,
    path: &str,
) -> Result<Vec<u8>, WsiError> {
    let stream = compound.open_stream(path)?;
    Ok(crate::core::limits::read_to_end_bounded(
        stream,
        crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
        "ZVI compound stream",
    )?)
}
