use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::registry::SlideReader;
use crate::core::types::{
    CpuTile, Dataset, LevelIdx, RawCompressedTile, SceneId, SeriesId, TileCodecKind, TileRequest,
};
use crate::error::WsiError;

use super::backend::is_encapsulated_transfer_syntax;
use super::manifest::DicomSlide;
use super::{
    HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX, HTJ2K_LOSSLESS_TRANSFER_SYNTAX, HTJ2K_TRANSFER_SYNTAX,
    JP2K_TRANSFER_SYNTAXES,
};

mod batch_plan;
mod cpu;
#[cfg(any(feature = "metal", feature = "cuda"))]
mod device;

pub(super) struct DicomReader {
    pub(super) slide: Arc<DicomSlide>,
}

impl SlideReader for DicomReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.slide
            .levels
            .get(req.level.get() as usize)
            .map(|level| level.tile_codec_kind(req))
            .unwrap_or(TileCodecKind::Other)
    }

    fn prepare_level_controlled(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
        control: &crate::ReadControl,
    ) -> Result<(), WsiError> {
        control.check_cancelled()?;
        let scene_ref =
            self.slide
                .dataset
                .scenes
                .get(scene.get())
                .ok_or(WsiError::SceneOutOfRange {
                    index: scene.get(),
                    count: self.slide.dataset.scenes.len(),
                })?;
        let series_ref = scene_ref
            .series
            .get(series.get())
            .ok_or(WsiError::SeriesOutOfRange {
                index: series.get(),
                count: scene_ref.series.len(),
            })?;
        let dicom_level =
            self.slide
                .levels
                .get(level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: level.get(),
                    count: series_ref.levels.len() as u32,
                })?;
        for image in &dicom_level.parts {
            control.check_cancelled()?;
            if is_encapsulated_transfer_syntax(&image.transfer_syntax_uid) {
                image.ensure_encapsulated_frames_controlled(Some(control))?;
            }
        }
        control.check_cancelled()
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_cpu_with_backend_controlled(reqs, BackendRequest::Cpu, None)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        let result =
            self.read_tiles_cpu_with_backend_controlled(reqs, BackendRequest::Cpu, Some(control));
        control.check_cancelled()?;
        result
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Cpu)
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.read_tiles_jp2k_metal(reqs, sessions)
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        self.read_tiles_jp2k_cuda(reqs, sessions)
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let image =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;
        image.read_raw_compressed_tile(req.col, req.row, req.level.get())
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let image = self
            .slide
            .associated
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        image.read_associated(name)
    }
}

pub(super) fn dicom_tile_codec_kind(transfer_syntax_uid: &str) -> TileCodecKind {
    if super::is_jpeg_transfer_syntax(transfer_syntax_uid) {
        TileCodecKind::Jpeg
    } else if matches!(
        transfer_syntax_uid,
        HTJ2K_TRANSFER_SYNTAX
            | HTJ2K_LOSSLESS_TRANSFER_SYNTAX
            | HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX
    ) {
        TileCodecKind::Htj2k
    } else if JP2K_TRANSFER_SYNTAXES.contains(&transfer_syntax_uid) {
        TileCodecKind::Jp2k
    } else {
        TileCodecKind::Other
    }
}
