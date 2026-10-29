// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where tiles' encoded bytes are stored, for byte-range references.

use pyo3::{
    exceptions::{PyMemoryError, PyValueError},
    prelude::*,
    pybacked::PyBackedBytes,
    types::{PyBytes, PyDict, PyList, PySequence, PyTuple},
};
use wsi_rs::{
    ByteOrder, ChannelOrder, EncodedCodec, EncodedUnit, LevelTileLocations, NotAddressableReason,
    PixelRect, PlanarConfiguration, Predictor, StoredFraming, TileLocation,
};

use super::tiles::{codec_name, photometric_name};
use super::wsi_error;

type Region = (u32, u32, u32, u32);

/// Where one tile's encoded bytes are stored, read from metadata only.
#[pyclass(name = "TileLocation", module = "wsi_rs", frozen)]
pub(super) struct PyTileLocation {
    inner: TileLocation,
}

impl PyTileLocation {
    pub(super) fn new(inner: TileLocation) -> Self {
        Self { inner }
    }

    fn not_addressable(&self) -> Option<NotAddressableReason> {
        match &self.inner {
            TileLocation::NotAddressable(reason) => Some(*reason),
            _ => None,
        }
    }

    fn synthetic(&self) -> Option<(u32, u32)> {
        match self.not_addressable() {
            Some(NotAddressableReason::SyntheticLevel { base_level, factor }) => {
                Some((base_level.get(), factor))
            }
            _ => None,
        }
    }
}

#[pymethods]
impl PyTileLocation {
    /// `stored`, `absent` or `not_addressable`.
    #[getter]
    fn kind(&self) -> &'static str {
        match &self.inner {
            TileLocation::Stored(_) => "stored",
            TileLocation::Absent => "absent",
            TileLocation::NotAddressable(_) => "not_addressable",
            _ => "unknown",
        }
    }

    /// Why a not-addressable tile has no byte ranges; `None` otherwise.
    #[getter]
    fn reason(&self) -> Option<&'static str> {
        self.not_addressable().map(|reason| match reason {
            NotAddressableReason::SyntheticLevel { .. } => "synthetic_level",
            NotAddressableReason::Resampled => "resampled",
            NotAddressableReason::CodestreamWindow => "codestream_window",
            NotAddressableReason::NotStored => "not_stored",
            _ => "unknown",
        })
    }

    /// Stored level that a synthetic level is computed from.
    #[getter]
    fn base_level(&self) -> Option<u32> {
        self.synthetic().map(|(base_level, _)| base_level)
    }

    /// Downsampling factor from `base_level` to a synthetic level.
    #[getter]
    fn factor(&self) -> Option<u32> {
        self.synthetic().map(|(_, factor)| factor)
    }

    /// Logical `(width, height)` of a stored tile, clipped at the level edge.
    #[getter]
    fn tile_size(&self) -> Option<(u32, u32)> {
        match &self.inner {
            TileLocation::Stored(stored) => Some(stored.tile_size()),
            _ => None,
        }
    }

    /// Stored encoded units that make up the tile; empty unless stored.
    #[getter]
    fn units<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let units: &[EncodedUnit] = match &self.inner {
            TileLocation::Stored(stored) => stored.units(),
            _ => &[],
        };
        PyTuple::new(
            py,
            units.iter().map(|unit| PyEncodedUnit {
                inner: unit.clone(),
            }),
        )
    }

    /// The location as plain Python values (JSON-compatible except bytes).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("kind", self.kind())?;
        match &self.inner {
            TileLocation::Stored(stored) => {
                let (width, height) = stored.tile_size();
                out.set_item("tile_size", [width, height])?;
                let units = PyList::empty(py);
                for unit in stored.units() {
                    units.append(unit_dict(py, unit)?)?;
                }
                out.set_item("units", units)?;
            }
            TileLocation::NotAddressable(_) => {
                out.set_item("reason", self.reason())?;
                if let Some((base_level, factor)) = self.synthetic() {
                    out.set_item("base_level", base_level)?;
                    out.set_item("factor", factor)?;
                }
            }
            _ => {}
        }
        Ok(out)
    }
}

/// One stored encoded image (a TIFF tile or strip, a DICOM frame, a JPEG
/// file) and the part of it that a tile uses.
#[pyclass(name = "EncodedUnit", module = "wsi_rs", frozen)]
pub(super) struct PyEncodedUnit {
    inner: EncodedUnit,
}

#[pymethods]
impl PyEncodedUnit {
    /// The slide file that holds the segments, as the reader opened it.
    #[getter]
    fn path<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(self.inner.path().as_os_str().into_pyobject(py)?.into_any())
    }

    /// `(offset, length)` byte ranges of the stored stream, in stream order.
    #[getter]
    fn segments(&self) -> Vec<(u64, u64)> {
        segments(&self.inner)
    }

    /// `standalone`, `jpeg_tables`, `bare_payload` or `prefixed`.
    #[getter]
    fn framing(&self) -> &'static str {
        framing_name(self.inner.framing())
    }

    #[getter]
    fn prefix<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.prefix())
    }

    #[getter]
    fn suffix<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.suffix())
    }

    /// Decoder parameters as a new dictionary.
    #[getter]
    fn codec<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        codec_dict(py, self.inner.codec())
    }

    /// `(width, height)` of the stored image, including edge padding.
    #[getter]
    fn stored_size(&self) -> (u32, u32) {
        self.inner.stored_size()
    }

    /// `(x, y, width, height)` of the stored image that the tile uses.
    #[getter]
    fn source_region(&self) -> Region {
        region(self.inner.source_region())
    }

    /// `(x, y, width, height)` where `source_region` lands in the tile.
    #[getter]
    fn tile_region(&self) -> Region {
        region(self.inner.tile_region())
    }

    /// Builds the standalone stream from the bytes at `segments`, read by
    /// the caller in the same order.
    fn assemble<'py>(
        &self,
        py: Python<'py>,
        parts: &Bound<'py, PySequence>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let expected = self.inner.segments().len();
        let count = parts.len()?;
        if count != expected {
            return Err(PyValueError::new_err(format!(
                "encoded unit has {expected} segments, but {count} were supplied"
            )));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(count)
            .map_err(|e| PyMemoryError::new_err(e.to_string()))?;
        for index in 0..count {
            let part = parts
                .get_item(index)
                .and_then(|item| item.extract::<PyBackedBytes>().map_err(PyErr::from))
                .map_err(|error| {
                    PyErr::from_type(error.get_type(py), format!("segment {index}: {error}"))
                })?;
            bytes.push(part);
        }
        let stream = py
            .detach(|| self.inner.assemble(bytes.iter().map(|part| &part[..])))
            .map_err(wsi_error)?;
        Ok(PyBytes::new(py, &stream))
    }

    /// The unit as plain Python values (JSON-compatible except bytes).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        unit_dict(py, &self.inner)
    }
}

/// Every tile location of one level plane, in row-major order.
#[pyclass(name = "LevelTileLocations", module = "wsi_rs", frozen, get_all)]
pub(super) struct PyLevelTileLocations {
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
    /// `(col, row, TileLocation)` entries.
    tiles: Py<PyTuple>,
}

impl PyLevelTileLocations {
    pub(super) fn new(py: Python<'_>, locations: LevelTileLocations) -> PyResult<Self> {
        let LevelTileLocations {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
            tiles,
            ..
        } = locations;
        let tiles = PyTuple::new(
            py,
            tiles
                .into_iter()
                .map(|tile| (tile.col, tile.row, PyTileLocation::new(tile.location))),
        )?;
        Ok(Self {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
            tiles: tiles.unbind(),
        })
    }
}

#[pymethods]
impl PyLevelTileLocations {
    /// The level as plain Python values (JSON-compatible except bytes);
    /// `tiles` holds `[col, row, location]` lists.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("tile_width", self.tile_width)?;
        out.set_item("tile_height", self.tile_height)?;
        out.set_item("tiles_across", self.tiles_across)?;
        out.set_item("tiles_down", self.tiles_down)?;
        let tiles = PyList::empty(py);
        for entry in self.tiles.bind(py).iter() {
            let (col, row, location) = entry.extract::<(i64, i64, Bound<'py, PyTileLocation>)>()?;
            let item = PyList::empty(py);
            item.append(col)?;
            item.append(row)?;
            item.append(location.get().to_dict(py)?)?;
            tiles.append(item)?;
        }
        out.set_item("tiles", tiles)?;
        Ok(out)
    }
}

fn unit_dict<'py>(py: Python<'py>, unit: &EncodedUnit) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("path", unit.path().as_os_str())?;
    let ranges = PyList::empty(py);
    for (offset, length) in segments(unit) {
        ranges.append([offset, length])?;
    }
    out.set_item("segments", ranges)?;
    out.set_item("framing", framing_name(unit.framing()))?;
    out.set_item("prefix", PyBytes::new(py, unit.prefix()))?;
    out.set_item("suffix", PyBytes::new(py, unit.suffix()))?;
    out.set_item("codec", codec_dict(py, unit.codec())?)?;
    let (width, height) = unit.stored_size();
    out.set_item("stored_size", [width, height])?;
    out.set_item("source_region", region_list(unit.source_region()))?;
    out.set_item("tile_region", region_list(unit.tile_region()))?;
    Ok(out)
}

fn codec_dict<'py>(py: Python<'py>, codec: &EncodedCodec) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("compression", codec_name(codec.compression()))?;
    out.set_item("bits_per_sample", codec.bits_per_sample())?;
    out.set_item("significant_bits", codec.significant_bits())?;
    out.set_item("samples_per_pixel", codec.samples_per_pixel())?;
    out.set_item(
        "photometric_interpretation",
        photometric_name(codec.photometric_interpretation()),
    )?;
    out.set_item(
        "planar_configuration",
        match codec.planar_configuration() {
            PlanarConfiguration::Chunky => "chunky",
            PlanarConfiguration::Planar => "planar",
            _ => "unknown",
        },
    )?;
    out.set_item(
        "channel_order",
        match codec.channel_order() {
            ChannelOrder::Rgb => "rgb",
            ChannelOrder::Bgr => "bgr",
            _ => "unknown",
        },
    )?;
    out.set_item(
        "predictor",
        match codec.predictor() {
            Predictor::None => "none",
            Predictor::Horizontal => "horizontal",
            Predictor::FloatingPoint => "floating_point",
            _ => "unknown",
        },
    )?;
    out.set_item(
        "byte_order",
        codec.byte_order().map(|order| match order {
            ByteOrder::LittleEndian => "little",
            ByteOrder::BigEndian => "big",
            _ => "unknown",
        }),
    )?;
    out.set_item(
        "chroma_subsampling",
        // Widened so the pair converts to a list, not bytes.
        codec
            .chroma_subsampling()
            .map(|(horizontal, vertical)| [u32::from(horizontal), u32::from(vertical)]),
    )?;
    out.set_item(
        "jpeg_tables",
        codec.jpeg_tables().map(|tables| PyBytes::new(py, tables)),
    )?;
    Ok(out)
}

fn framing_name(framing: StoredFraming) -> &'static str {
    match framing {
        StoredFraming::Standalone => "standalone",
        StoredFraming::JpegTables => "jpeg_tables",
        StoredFraming::BarePayload => "bare_payload",
        StoredFraming::Prefixed => "prefixed",
        _ => "unknown",
    }
}

fn segments(unit: &EncodedUnit) -> Vec<(u64, u64)> {
    unit.segments()
        .iter()
        .map(|range| (range.offset, range.length))
        .collect()
}

fn region(rect: PixelRect) -> Region {
    (rect.x, rect.y, rect.width, rect.height)
}

fn region_list(rect: PixelRect) -> [u32; 4] {
    [rect.x, rect.y, rect.width, rect.height]
}
