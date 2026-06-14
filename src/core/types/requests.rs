use std::{error::Error, fmt};

use super::*;

// ── Request types ──────────────────────────────────────────────────

/// Selects a z/c/t plane. Default is (0,0,0) for plain 2D reads.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Default)]
#[non_exhaustive]
pub struct PlaneSelection {
    pub z: u32,
    pub c: u32,
    pub t: u32,
}

impl PlaneSelection {
    pub const fn new(z: u32, c: u32, t: u32) -> Self {
        Self { z, c, t }
    }
}

/// A region request — used by Slide (public API), not by backends.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RegionRequest {
    pub scene: SceneId,
    pub series: SeriesId,
    pub level: LevelIdx,
    pub plane: PlaneIdx,
    /// Signed top-left pixel position in level coordinates.
    pub origin_px: (i64, i64),
    /// Unsigned region size in pixels.
    pub size_px: (u32, u32),
}

impl RegionRequest {
    /// Build a region request for the default z/c/t plane.
    pub fn new(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
        origin_px: (i64, i64),
        size_px: (u32, u32),
    ) -> Self {
        Self {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            origin_px,
            size_px,
        }
    }

    /// Start a named-field builder for a region request.
    pub fn builder(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
    ) -> RegionRequestBuilder {
        RegionRequestBuilder {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            origin_px: None,
            size_px: None,
        }
    }

    /// Return a copy of this request targeting a specific z/c/t plane.
    #[must_use]
    pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }
}

/// Builder for [`RegionRequest`].
#[derive(Debug, Clone)]
#[must_use]
pub struct RegionRequestBuilder {
    scene: SceneId,
    series: SeriesId,
    level: LevelIdx,
    plane: PlaneIdx,
    origin_px: Option<(i64, i64)>,
    size_px: Option<(u32, u32)>,
}

impl RegionRequestBuilder {
    /// Set the signed top-left pixel position in level coordinates.
    pub fn origin_px(mut self, origin_px: (i64, i64)) -> Self {
        self.origin_px = Some(origin_px);
        self
    }

    /// Set the unsigned region size in pixels.
    pub fn size_px(mut self, size_px: (u32, u32)) -> Self {
        self.size_px = Some(size_px);
        self
    }

    /// Set the target z/c/t plane.
    pub fn plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }

    /// Build the request after all required fields have been provided.
    pub fn build(self) -> Result<RegionRequest, RequestBuildError> {
        let origin_px = self.origin_px.ok_or(RequestBuildError::MissingOrigin)?;
        let size_px = self.size_px.ok_or(RequestBuildError::MissingSize)?;
        Ok(RegionRequest {
            scene: self.scene,
            series: self.series,
            level: self.level,
            plane: self.plane,
            origin_px,
            size_px,
        })
    }
}

/// A single-tile request — the backend primitive.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TileRequest {
    pub scene: SceneId,
    pub series: SeriesId,
    pub level: LevelIdx,
    pub plane: PlaneIdx,
    pub col: i64,
    pub row: i64,
}

impl TileRequest {
    /// Build a source tile request for the default z/c/t plane.
    pub fn new(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
        col: i64,
        row: i64,
    ) -> Self {
        Self {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            col,
            row,
        }
    }

    /// Start a named-field builder for a source tile request.
    pub fn builder(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
    ) -> TileRequestBuilder {
        TileRequestBuilder {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            col: None,
            row: None,
        }
    }

    /// Return a copy of this request targeting a specific z/c/t plane.
    #[must_use]
    pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }
}

/// Builder for [`TileRequest`].
#[derive(Debug, Clone)]
#[must_use]
pub struct TileRequestBuilder {
    scene: SceneId,
    series: SeriesId,
    level: LevelIdx,
    plane: PlaneIdx,
    col: Option<i64>,
    row: Option<i64>,
}

impl TileRequestBuilder {
    /// Set the storage-native tile column and row.
    pub fn tile(mut self, col: i64, row: i64) -> Self {
        self.col = Some(col);
        self.row = Some(row);
        self
    }

    /// Set the storage-native tile column.
    pub fn col(mut self, col: i64) -> Self {
        self.col = Some(col);
        self
    }

    /// Set the storage-native tile row.
    pub fn row(mut self, row: i64) -> Self {
        self.row = Some(row);
        self
    }

    /// Set the target z/c/t plane.
    pub fn plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }

    /// Build the request after all required fields have been provided.
    pub fn build(self) -> Result<TileRequest, RequestBuildError> {
        let col = self.col.ok_or(RequestBuildError::MissingColumn)?;
        let row = self.row.ok_or(RequestBuildError::MissingRow)?;
        Ok(TileRequest {
            scene: self.scene,
            series: self.series,
            level: self.level,
            plane: self.plane,
            col,
            row,
        })
    }
}

/// A viewer/display-tile request.
///
/// Unlike `TileRequest`, this is expressed in the viewer's regular display grid
/// rather than the storage-native tile layout. Backends may satisfy it from
/// native tiles, cached decode bands, or region composition as appropriate.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TileViewRequest {
    pub scene: SceneId,
    pub series: SeriesId,
    pub level: LevelIdx,
    pub plane: PlaneIdx,
    pub col: i64,
    pub row: i64,
    pub tile_width: u32,
    pub tile_height: u32,
}

impl TileViewRequest {
    /// Build a display-grid tile request for the default z/c/t plane.
    pub fn new(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
        col: i64,
        row: i64,
        tile_width: u32,
        tile_height: u32,
    ) -> Self {
        Self {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            col,
            row,
            tile_width,
            tile_height,
        }
    }

    /// Start a named-field builder for a display-grid tile request.
    pub fn builder(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
    ) -> TileViewRequestBuilder {
        TileViewRequestBuilder {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            col: None,
            row: None,
            tile_width: None,
            tile_height: None,
        }
    }

    /// Return a copy of this request targeting a specific z/c/t plane.
    #[must_use]
    pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }
}

/// Builder for [`TileViewRequest`].
#[derive(Debug, Clone)]
#[must_use]
pub struct TileViewRequestBuilder {
    scene: SceneId,
    series: SeriesId,
    level: LevelIdx,
    plane: PlaneIdx,
    col: Option<i64>,
    row: Option<i64>,
    tile_width: Option<u32>,
    tile_height: Option<u32>,
}

impl TileViewRequestBuilder {
    /// Set the display-grid tile column and row.
    pub fn tile(mut self, col: i64, row: i64) -> Self {
        self.col = Some(col);
        self.row = Some(row);
        self
    }

    /// Set the display-grid tile column.
    pub fn col(mut self, col: i64) -> Self {
        self.col = Some(col);
        self
    }

    /// Set the display-grid tile row.
    pub fn row(mut self, row: i64) -> Self {
        self.row = Some(row);
        self
    }

    /// Set the display-grid tile dimensions in pixels.
    pub fn tile_size(mut self, tile_width: u32, tile_height: u32) -> Self {
        self.tile_width = Some(tile_width);
        self.tile_height = Some(tile_height);
        self
    }

    /// Set the display-grid tile width in pixels.
    pub fn tile_width(mut self, tile_width: u32) -> Self {
        self.tile_width = Some(tile_width);
        self
    }

    /// Set the display-grid tile height in pixels.
    pub fn tile_height(mut self, tile_height: u32) -> Self {
        self.tile_height = Some(tile_height);
        self
    }

    /// Set the target z/c/t plane.
    pub fn plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }

    /// Build the request after all required fields have been provided.
    pub fn build(self) -> Result<TileViewRequest, RequestBuildError> {
        let col = self.col.ok_or(RequestBuildError::MissingColumn)?;
        let row = self.row.ok_or(RequestBuildError::MissingRow)?;
        let tile_width = self.tile_width.ok_or(RequestBuildError::MissingTileWidth)?;
        let tile_height = self
            .tile_height
            .ok_or(RequestBuildError::MissingTileHeight)?;
        Ok(TileViewRequest {
            scene: self.scene,
            series: self.series,
            level: self.level,
            plane: self.plane,
            col,
            row,
            tile_width,
            tile_height,
        })
    }
}

/// Error returned by public request builders when a required field is missing.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum RequestBuildError {
    /// `RegionRequestBuilder::origin_px` was not provided.
    MissingOrigin,
    /// `RegionRequestBuilder::size_px` was not provided.
    MissingSize,
    /// `TileRequestBuilder::col` or `TileViewRequestBuilder::col` was not provided.
    MissingColumn,
    /// `TileRequestBuilder::row` or `TileViewRequestBuilder::row` was not provided.
    MissingRow,
    /// `TileViewRequestBuilder::tile_width` was not provided.
    MissingTileWidth,
    /// `TileViewRequestBuilder::tile_height` was not provided.
    MissingTileHeight,
}

impl fmt::Display for RequestBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingOrigin => "region request origin is required",
            Self::MissingSize => "region request size is required",
            Self::MissingColumn => "tile column is required",
            Self::MissingRow => "tile row is required",
            Self::MissingTileWidth => "display tile width is required",
            Self::MissingTileHeight => "display tile height is required",
        };
        f.write_str(message)
    }
}

impl Error for RequestBuildError {}
