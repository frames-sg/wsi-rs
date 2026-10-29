// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only Python access to slide metadata, tile geometry, encoded payloads,
//! and the byte locations of stored tiles.

#![cfg(feature = "python")]
#![deny(unsafe_code)]

use std::path::PathBuf;

use pyo3::{
    exceptions::{PyMemoryError, PyOSError, PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PySequence},
};
use wsi_rs::{Level, PlaneSelection, Series, Slide, TileRequest, WsiError};

mod locations;
mod metadata;
mod tiles;

use locations::{PyEncodedUnit, PyLevelTileLocations, PyTileLocation};
use tiles::{CompressedTile, TileHit};

/// An open reader. Construct one inside each DataLoader worker.
#[pyclass(name = "Slide", module = "wsi_rs", frozen)]
struct PySlide {
    inner: Slide,
    process_id: u32,
}

#[pymethods]
impl PySlide {
    #[new]
    fn new(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| Slide::open(path).map_err(wsi_error))?;
        Ok(Self {
            inner,
            process_id: std::process::id(),
        })
    }

    /// Open a slide file or DICOM directory without decoding its pixels.
    #[staticmethod]
    fn open(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        Self::new(py, path)
    }

    /// Scanner properties as an independent Python dictionary.
    #[getter]
    fn properties<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.check_process()?;
        metadata::properties(py, &self.inner.dataset().properties)
    }

    /// Scenes, series, axes, channels, levels, associated-image metadata, and properties.
    #[getter]
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.check_process()?;
        metadata::dataset(py, self.inner.dataset())
    }

    /// Pyramid levels, with native or virtual tile-grid metadata.
    #[pyo3(signature = (*, scene=0, series=0))]
    fn levels<'py>(
        &self,
        py: Python<'py>,
        scene: usize,
        series: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        self.check_process()?;
        metadata::levels(py, &self.series(scene, series)?.levels)
    }

    /// Tiles covering a rectangle in the selected level's pixel coordinates.
    /// Offsets place each tile relative to the requested rectangle's origin.
    #[pyo3(signature = (region, *, level=0, scene=0, series=0))]
    fn tiles_for_region(
        &self,
        py: Python<'_>,
        region: (i64, i64, u32, u32),
        level: u32,
        scene: usize,
        series: usize,
    ) -> PyResult<Vec<TileHit>> {
        self.check_process()?;
        let level = self.level(scene, series, level)?;
        py.detach(|| {
            tiles::validate_region(&level.tile_layout, region, self.inner.limits())?;
            let (x, y, width, height) = region;
            Ok(level
                .tile_layout
                .tiles_for_region(x, y, width, height)
                .into_iter()
                .map(|hit| TileHit {
                    coords: (hit.col, hit.row),
                    offset: (hit.dest_x_f64, hit.dest_y_f64),
                })
                .collect())
        })
    }

    /// Read compressed tiles in caller order, preserving duplicates.
    /// Tile read failures name the input index; no partial list is returned.
    #[pyo3(signature = (coords, *, level=0, scene=0, series=0, plane=(0, 0, 0)))]
    fn read_tiles_compressed(
        &self,
        py: Python<'_>,
        coords: &Bound<'_, PySequence>,
        level: u32,
        scene: usize,
        series: usize,
        plane: (u32, u32, u32),
    ) -> PyResult<Vec<CompressedTile>> {
        self.check_process()?;
        let count = coords.len()?;
        let limits = self.inner.limits();
        // Extraction, native reads, and Python conversion can retain all three
        // Rust vectors at once. Check their combined metadata before allocation.
        let item_bytes = std::mem::size_of::<(i64, i64)>()
            + std::mem::size_of::<wsi_rs::RawCompressedTile>()
            + std::mem::size_of::<CompressedTile>();
        tiles::check_budget(
            count as u64,
            item_bytes as u64,
            limits.tile_index_bytes(),
            "compressed batch metadata",
        )?;
        let plane = self.plane(scene, series, level, plane)?;
        let mut coordinates = Vec::new();
        coordinates
            .try_reserve_exact(count)
            .map_err(|e| PyMemoryError::new_err(e.to_string()))?;
        for index in 0..count {
            let coordinate = coords
                .get_item(index)
                .and_then(|item| item.extract::<(i64, i64)>())
                .map_err(|error| {
                    PyErr::from_type(error.get_type(py), format!("input {index}: {error}"))
                })?;
            coordinates.push(coordinate);
        }
        let raw = py.detach(|| {
            let mut output = Vec::new();
            output
                .try_reserve_exact(count)
                .map_err(|e| PyMemoryError::new_err(e.to_string()))?;
            let mut bytes = 0_u64;
            for (index, &(col, row)) in coordinates.iter().enumerate() {
                let request = TileRequest::new(scene, series, level, col, row).with_plane(plane);
                let tile = self.inner.read_raw_compressed_tile(&request).map_err(|e| {
                    let message = format!("input {index}: {e}");
                    if matches!(e, WsiError::Io(_) | WsiError::IoWithPath { .. }) {
                        PyOSError::new_err(message)
                    } else {
                        PyValueError::new_err(message)
                    }
                })?;
                bytes = bytes
                    .checked_add(tile.data().len() as u64)
                    .ok_or_else(|| PyValueError::new_err("compressed batch size overflow"))?;
                tiles::check_budget(
                    bytes,
                    1,
                    limits.batch_chunk_bytes(),
                    "compressed batch bytes; request fewer tiles",
                )?;
                output.push(tile);
            }
            Ok::<_, PyErr>(output)
        })?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(count)
            .map_err(|e| PyMemoryError::new_err(e.to_string()))?;
        for (tile, coords) in raw.into_iter().zip(coordinates) {
            output.push(CompressedTile::new(py, tile, coords)?);
        }
        Ok(output)
    }

    /// Where one tile's encoded bytes are stored, read from metadata only.
    #[pyo3(signature = (col, row, *, level=0, scene=0, series=0, plane=(0, 0, 0)))]
    #[allow(clippy::too_many_arguments)]
    fn tile_location(
        &self,
        py: Python<'_>,
        col: i64,
        row: i64,
        level: u32,
        scene: usize,
        series: usize,
        plane: (u32, u32, u32),
    ) -> PyResult<PyTileLocation> {
        self.check_process()?;
        let plane = self.plane(scene, series, level, plane)?;
        let request = TileRequest::new(scene, series, level, col, row).with_plane(plane);
        let location = py
            .detach(|| self.inner.tile_location(&request))
            .map_err(wsi_error)?;
        Ok(PyTileLocation::new(location))
    }

    /// Every tile location of one level plane in row-major order. Bounded by
    /// the reader's tile-index limit; a failing tile fails the whole call.
    #[pyo3(signature = (*, level=0, scene=0, series=0, plane=(0, 0, 0)))]
    fn level_tile_locations(
        &self,
        py: Python<'_>,
        level: u32,
        scene: usize,
        series: usize,
        plane: (u32, u32, u32),
    ) -> PyResult<PyLevelTileLocations> {
        self.check_process()?;
        let plane = self.plane(scene, series, level, plane)?;
        let locations = py
            .detach(|| self.inner.level_tile_locations(scene, series, level, plane))
            .map_err(wsi_error)?;
        PyLevelTileLocations::new(py, locations)
    }
}

impl PySlide {
    fn check_process(&self) -> PyResult<()> {
        if self.process_id != std::process::id() {
            return Err(PyRuntimeError::new_err(
                "slide was inherited across fork; open a new Slide inside each DataLoader worker",
            ));
        }
        Ok(())
    }

    fn series(&self, scene: usize, series: usize) -> PyResult<&Series> {
        let scenes = &self.inner.dataset().scenes;
        let selected = scenes.get(scene).ok_or_else(|| {
            wsi_error(WsiError::SceneOutOfRange {
                index: scene,
                count: scenes.len(),
            })
        })?;
        selected.series.get(series).ok_or_else(|| {
            wsi_error(WsiError::SeriesOutOfRange {
                index: series,
                count: selected.series.len(),
            })
        })
    }

    /// Checks the level and `(z, c, t)` plane before any tile work.
    fn plane(
        &self,
        scene: usize,
        series: usize,
        level: u32,
        (z, c, t): (u32, u32, u32),
    ) -> PyResult<PlaneSelection> {
        self.level(scene, series, level)?;
        let axes = self.series(scene, series)?.axes;
        for (axis, value, extent) in [("z", z, axes.z), ("c", c, axes.c), ("t", t, axes.t)] {
            if value >= extent {
                return Err(wsi_error(WsiError::PlaneOutOfRange {
                    axis: axis.into(),
                    value,
                    max: extent.saturating_sub(1),
                }));
            }
        }
        Ok(PlaneSelection::new(z, c, t))
    }

    fn level(&self, scene: usize, series: usize, level: u32) -> PyResult<&Level> {
        let levels = &self.series(scene, series)?.levels;
        levels.get(level as usize).ok_or_else(|| {
            wsi_error(WsiError::LevelOutOfRange {
                level,
                count: u32::try_from(levels.len()).unwrap_or(u32::MAX),
            })
        })
    }
}

fn wsi_error(error: WsiError) -> PyErr {
    if matches!(error, WsiError::Io(_) | WsiError::IoWithPath { .. }) {
        PyOSError::new_err(error.to_string())
    } else {
        PyValueError::new_err(error.to_string())
    }
}

#[pymodule]
#[pyo3(name = "wsi_rs")]
fn wsi_rs_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_class::<PySlide>()?;
    module.add_class::<CompressedTile>()?;
    module.add_class::<TileHit>()?;
    module.add_class::<PyTileLocation>()?;
    module.add_class::<PyEncodedUnit>()?;
    module.add_class::<PyLevelTileLocations>()?;
    Ok(())
}
