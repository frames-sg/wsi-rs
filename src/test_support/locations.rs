use crate::{ByteRange, EncodedUnit, StoredTile, TileLocation};

pub(crate) fn stored_tile(location: &TileLocation) -> &StoredTile {
    match location {
        TileLocation::Stored(stored) => stored,
        other => panic!("expected a stored tile, got {other:?}"),
    }
}

pub(crate) fn single_unit(location: &TileLocation) -> &EncodedUnit {
    stored_tile(location)
        .single_unit()
        .expect("tile is stored in one unit")
}

pub(crate) fn assemble_with(
    unit: &EncodedUnit,
    read: impl FnMut(&ByteRange) -> Vec<u8>,
) -> Vec<u8> {
    let segments: Vec<_> = unit.segments().iter().map(read).collect();
    unit.assemble(segments.iter().map(Vec::as_slice)).unwrap()
}

pub(crate) fn assembled(unit: &EncodedUnit) -> Vec<u8> {
    let file = std::fs::read(unit.path()).expect("read located file");
    assemble_with(unit, |range| {
        file[range.offset as usize..][..range.length as usize].to_vec()
    })
}
