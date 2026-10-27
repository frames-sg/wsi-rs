// SPDX-License-Identifier: MIT OR Apache-2.0

use pyo3::{
    prelude::*,
    types::{PyDict, PyList},
};
use wsi_rs::{Dataset, Level, Properties, TileLayout};

pub(super) fn properties<'py>(
    py: Python<'py>,
    properties: &Properties,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    for (key, value) in properties.iter() {
        out.set_item(key, value)?;
    }
    Ok(out)
}

pub(super) fn levels<'py>(py: Python<'py>, levels: &[Level]) -> PyResult<Vec<Bound<'py, PyDict>>> {
    levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            let out = PyDict::new(py);
            out.set_item("index", index)?;
            out.set_item("dimensions", level.dimensions)?;
            out.set_item("downsample", level.downsample)?;
            out.set_item("tile_grid", grid(py, &level.tile_layout)?)?;
            Ok(out)
        })
        .collect()
}

fn grid<'py>(py: Python<'py>, layout: &TileLayout) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    match layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => {
            out.set_item("kind", "regular")?;
            out.set_item("tile_size", (*tile_width, *tile_height))?;
            out.set_item("grid_size", (*tiles_across, *tiles_down))?;
        }
        TileLayout::WholeLevel {
            width,
            height,
            virtual_tile_width,
            virtual_tile_height,
        } => {
            out.set_item("kind", "virtual")?;
            out.set_item("tile_size", (*virtual_tile_width, *virtual_tile_height))?;
            out.set_item(
                "grid_size",
                (
                    width.div_ceil(u64::from(*virtual_tile_width).max(1)),
                    height.div_ceil(u64::from(*virtual_tile_height).max(1)),
                ),
            )?;
        }
        TileLayout::Irregular {
            tile_advance,
            extra_tiles,
            tiles,
        } => {
            out.set_item("kind", "irregular")?;
            out.set_item("tile_advance", *tile_advance)?;
            out.set_item("extra_tiles", *extra_tiles)?;
            out.set_item("tile_count", tiles.len())?;
        }
        _ => {
            out.set_item("kind", "unknown")?;
        }
    }
    Ok(out)
}

pub(super) fn dataset<'py>(py: Python<'py>, dataset: &Dataset) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("properties", properties(py, &dataset.properties)?)?;
    let scenes = PyList::empty(py);
    for scene in &dataset.scenes {
        let item = PyDict::new(py);
        item.set_item("id", &scene.id)?;
        item.set_item("name", &scene.name)?;
        let series = PyList::empty(py);
        for s in &scene.series {
            let item = PyDict::new(py);
            item.set_item("id", &s.id)?;
            item.set_item("axes", (s.axes.z, s.axes.c, s.axes.t))?;
            item.set_item("sample_type", format!("{:?}", s.sample_type))?;
            item.set_item("levels", levels(py, &s.levels)?)?;
            let channels = PyList::empty(py);
            for c in &s.channels {
                let channel = PyDict::new(py);
                channel.set_item("name", &c.name)?;
                channel.set_item("color", c.color)?;
                channel.set_item("excitation_nm", c.excitation_nm)?;
                channel.set_item("emission_nm", c.emission_nm)?;
                channels.append(channel)?;
            }
            item.set_item("channels", channels)?;
            series.append(item)?;
        }
        item.set_item("series", series)?;
        scenes.append(item)?;
    }
    out.set_item("scenes", scenes)?;
    let associated = PyDict::new(py);
    for (name, image) in &dataset.associated_images {
        let item = PyDict::new(py);
        item.set_item("dimensions", image.dimensions)?;
        associated.set_item(name, item)?;
    }
    out.set_item("associated_images", associated)?;
    Ok(out)
}
