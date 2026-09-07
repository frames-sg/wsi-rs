#!/usr/bin/env python3
"""Decode public ZVI/VSI probes with the independent SlideIO 2.9.0 reader.

Run in an isolated environment:
uv run --python 3.13 --with slideio==2.9.0 scripts/prepare-vendor-reference.py
    --zvi /path/to/Zeiss-1-Merged.zvi --vsi /path/to/OS-1.vsi
    --output target/vendor-reference

Then set WSI_RS_VENDOR_REFERENCE_ROOT to that output directory and run:
cargo test --locked --test vendor_reference -- --ignored

The output stays local: cases.json records source paths and the raw files contain
native reference pixels. Source files are never modified or uploaded. These
probes cover channels, an interior tile boundary, tissue, and image edges; they
do not establish whole-image or pyramid parity or authorize redistribution.
"""

import argparse
import importlib.metadata
import json
from pathlib import Path

import slideio


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zvi", required=True, type=Path)
    parser.add_argument("--vsi", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if importlib.metadata.version("slideio") != "2.9.0":
        raise ValueError("reference generation requires SlideIO 2.9.0")
    args.output.mkdir(parents=True, exist_ok=True)
    cases = []
    for driver, path, dimensions, positions in (
        ("ZVI", args.zvi, (1480, 1132), [(0, 0), (700, 500), (1416, 1068)]),
        ("VSI", args.vsi, (66982, 76963),
         [(0, 0), (250, 250), (33400, 38400), (66918, 76899)]),
    ):
        path = path.resolve(strict=True)
        slide = slideio.open_slide(str(path), driver)
        scene = slide.get_scene(0)
        if scene.rect != (0, 0, *dimensions) or scene.num_channels != 3:
            raise ValueError("unexpected public sample geometry: " + str(path))
        for index, (x, y) in enumerate(positions):
            for channel in (range(3) if driver == "ZVI" else [0]):
                name = f"{driver.lower()}-{index}-{channel}.raw"
                pixels = scene.read_block(
                    (x, y, 64, 64),
                    channel_indices=[channel] if driver == "ZVI" else None,
                )
                # SlideIO exposes ZVI's 16-bit containers as int16. Preserve
                # those sample-code bits in explicitly little-endian U16 bytes.
                pixels = pixels.astype("<u2" if driver == "ZVI" else "u1")
                (args.output / name).write_bytes(pixels.tobytes())
                cases.append({
                    "path": str(path), "origin": [x, y], "size": [64, 64],
                    "channel": channel, "sample_type": "u16" if driver == "ZVI" else "u8",
                    "pixels": name,
                })
    (args.output / "cases.json").write_text(json.dumps(cases, indent=2) + "\n")
    print(f"Generated {len(cases)} independent native-pixel probes with SlideIO 2.9.0")


if __name__ == "__main__":
    main()
