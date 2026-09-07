//! Composition tile resolution retains the enclosing operation's admission.
use super::*;

pub(super) struct AdmittedReader<'a, 'b> {
    pub(super) source: &'a dyn ManagedSlideReader,
    pub(super) execution: &'a ReadExecutionContext<'b>,
}

impl SlideReader for AdmittedReader<'_, '_> {
    fn dataset(&self) -> &Dataset {
        self.source.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.source.tile_codec_kind(req)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        crate::core::batch::exactly_one(
            self.read_tiles_cpu(std::slice::from_ref(req))?,
            "composition admitted tile",
        )
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.source.read_tiles_with_context(reqs, self.execution)
    }
}
