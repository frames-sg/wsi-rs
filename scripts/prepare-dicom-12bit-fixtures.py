#!/usr/bin/env python3
"""Generate synthetic 12-bit JPEG DICOM WSI fixtures and independent references.

Run with uv run --with pydicom==3.0.2 scripts/prepare-dicom-12bit-fixtures.py.
Requires cjpeg and djpeg from libjpeg-turbo 3.x on PATH. Pixels are synthetic
and deterministic. Every frame is encoded by libjpeg-turbo's cjpeg with
-precision 12, and the references are libjpeg-turbo djpeg -rgb output, never
J2K or wsi-rs output. Each fixture is a 40x28 TILED_FULL matrix of six 16x16
frames, so the right and bottom tiles are partial.
"""

import argparse
import subprocess
import tempfile
import uuid
from pathlib import Path

from pydicom import dcmread
from pydicom.dataset import FileDataset, FileMetaDataset
from pydicom.encaps import encapsulate, generate_frames
from pydicom.uid import VLWholeSlideMicroscopyImageStorage

MATRIX_WIDTH = 40
MATRIX_HEIGHT = 28
TILE = 16
TILES_ACROSS = -(-MATRIX_WIDTH // TILE)
TILES_DOWN = -(-MATRIX_HEIGHT // TILE)
MAX_SAMPLE = 4095
EXTENDED = "1.2.840.10008.1.2.4.51"
FULL_PROGRESSION = "1.2.840.10008.1.2.4.55"

# (name, photometric, cjpeg sampling arguments, progressive, transfer syntax)
VARIANTS = (
    ("mono2-extended", "MONOCHROME2", ["-grayscale"], False, EXTENDED),
    ("mono2-progressive", "MONOCHROME2", ["-grayscale"], True, FULL_PROGRESSION),
    ("ybr422-extended", "YBR_FULL_422", ["-sample", "2x1,1x1,1x1"], False, EXTENDED),
    ("ybr422-progressive", "YBR_FULL_422", ["-sample", "2x1,1x1,1x1"], True, FULL_PROGRESSION),
)


def uid(name):
    return "2.25." + str(uuid.uuid5(uuid.NAMESPACE_URL, "wsi-rs/dicom-12bit-fixture/" + name).int)


def source_rgb(x, y):
    # Smooth ramps across the full 12-bit range plus fine detail, so that
    # truncating to 8 bits or dropping low bits changes the decoded samples.
    red = x * MAX_SAMPLE // (TILES_ACROSS * TILE - 1)
    green = y * MAX_SAMPLE // (TILES_DOWN * TILE - 1)
    blue = ((x + y) * 97 + (x * y) % 251) % (MAX_SAMPLE + 1)
    return red, green, blue


def source_gray(x, y):
    red, green, blue = source_rgb(x, y)
    return (red * 3 + green * 6 + blue) // 10


def write_netpbm(path, width, height, samples):
    magic = b"P6" if len(samples[0]) == 3 else b"P5"
    body = bytearray()
    for pixel in samples:
        for value in pixel:
            body += value.to_bytes(2, "big")
    path.write_bytes(magic + f"\n{width} {height}\n{MAX_SAMPLE}\n".encode() + bytes(body))


def read_ppm16(path):
    data = path.read_bytes()
    fields = []
    offset = 0
    while len(fields) < 4:
        while data[offset:offset + 1].isspace():
            offset += 1
        start = offset
        while not data[offset:offset + 1].isspace():
            offset += 1
        fields.append(data[start:offset])
    offset += 1
    magic, width, height, maxval = fields[0], int(fields[1]), int(fields[2]), int(fields[3])
    if magic != b"P6" or maxval != MAX_SAMPLE:
        raise ValueError(f"{path}: expected 12-bit P6 output from djpeg -rgb, got {magic} maxval {maxval}")
    body = data[offset:]
    if len(body) != width * height * 6:
        raise ValueError(f"{path}: truncated 16-bit PPM body")
    values = [int.from_bytes(body[i:i + 2], "big") for i in range(0, len(body), 2)]
    return width, height, values


def require_libjpeg_turbo_3():
    version = subprocess.run(["cjpeg", "-version"], capture_output=True, text=True, check=True)
    text = (version.stdout + version.stderr).strip()
    if "libjpeg-turbo version 3." not in text:
        raise SystemExit(f"cjpeg must come from libjpeg-turbo 3.x, got: {text}")
    return text


def encode_frames(workdir, photometric, sampling, progressive):
    frames = []
    reference = [0] * (MATRIX_WIDTH * MATRIX_HEIGHT * 3)
    for tile_row in range(TILES_DOWN):
        for tile_col in range(TILES_ACROSS):
            stem = workdir / f"tile-{tile_col}-{tile_row}"
            origin_x, origin_y = tile_col * TILE, tile_row * TILE
            coords = [(origin_x + x, origin_y + y) for y in range(TILE) for x in range(TILE)]
            if photometric == "MONOCHROME2":
                source = stem.with_suffix(".pgm")
                write_netpbm(source, TILE, TILE, [(source_gray(x, y),) for x, y in coords])
            else:
                source = stem.with_suffix(".ppm")
                write_netpbm(source, TILE, TILE, [source_rgb(x, y) for x, y in coords])
            encoded = stem.with_suffix(".jpg")
            decoded = stem.with_suffix(".ref.ppm")
            command = ["cjpeg", "-precision", "12", "-quality", "92", *sampling]
            if progressive:
                command.append("-progressive")
            subprocess.run([*command, "-outfile", str(encoded), str(source)], check=True)
            subprocess.run(["djpeg", "-rgb", "-outfile", str(decoded), str(encoded)], check=True)
            width, height, values = read_ppm16(decoded)
            if (width, height) != (TILE, TILE):
                raise ValueError(f"{decoded}: expected {TILE}x{TILE}, got {width}x{height}")
            if max(values) <= 255:
                raise ValueError(f"{decoded}: 12-bit decode has no samples above 8-bit range")
            for y in range(TILE):
                for x in range(TILE):
                    matrix_x, matrix_y = origin_x + x, origin_y + y
                    if matrix_x >= MATRIX_WIDTH or matrix_y >= MATRIX_HEIGHT:
                        continue
                    src = (y * TILE + x) * 3
                    dst = (matrix_y * MATRIX_WIDTH + matrix_x) * 3
                    reference[dst:dst + 3] = values[src:src + 3]
            frames.append(encoded.read_bytes())
    return frames, reference


def make_dicom(name, photometric, transfer_syntax, frames, destination):
    meta = FileMetaDataset()
    meta.TransferSyntaxUID = transfer_syntax
    meta.MediaStorageSOPClassUID = VLWholeSlideMicroscopyImageStorage
    meta.MediaStorageSOPInstanceUID = uid(name)
    dataset = FileDataset(None, {}, file_meta=meta, preamble=bytes(128))
    dataset.SOPClassUID = VLWholeSlideMicroscopyImageStorage
    dataset.SOPInstanceUID = uid(name)
    dataset.StudyInstanceUID = uid("study")
    dataset.SeriesInstanceUID = uid(name + "/series")
    dataset.FrameOfReferenceUID = uid(name + "/frame")
    dataset.Modality = "SM"
    dataset.PatientName = "SYNTHETIC^FIXTURE"
    dataset.PatientID = "SYNTHETIC-FIXTURE"
    dataset.ImageType = ["DERIVED", "PRIMARY", "VOLUME", "NONE"]
    dataset.Rows = TILE
    dataset.Columns = TILE
    dataset.SamplesPerPixel = 1 if photometric == "MONOCHROME2" else 3
    dataset.PhotometricInterpretation = photometric
    if dataset.SamplesPerPixel == 3:
        dataset.PlanarConfiguration = 0
    dataset.BitsAllocated = 16
    dataset.BitsStored = 12
    dataset.HighBit = 11
    dataset.PixelRepresentation = 0
    dataset.LossyImageCompression = "01"
    dataset.DimensionOrganizationType = "TILED_FULL"
    dataset.NumberOfFrames = len(frames)
    dataset.TotalPixelMatrixColumns = MATRIX_WIDTH
    dataset.TotalPixelMatrixRows = MATRIX_HEIGHT
    dataset.TotalPixelMatrixFocalPlanes = 1
    dataset.NumberOfOpticalPaths = 1
    dataset.PixelSpacing = [1, 1]  # Synthetic geometry: these are codec fixtures.
    dataset.PixelData = encapsulate(frames)
    dataset[0x7FE00010].is_undefined_length = True
    dataset.save_as(destination, enforce_file_format=True)
    recovered = dcmread(destination)
    # Encapsulation pads odd-length frames with one trailing zero byte.
    padded = [frame + b"\0" * (len(frame) % 2) for frame in frames]
    if list(generate_frames(recovered.PixelData, number_of_frames=len(frames))) != padded:
        raise ValueError(f"{destination}: encapsulated frames do not round-trip")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path,
                        default=Path(__file__).resolve().parents[1] / "tests/fixtures/dicom_12bit")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    print(require_libjpeg_turbo_3())
    for name, photometric, sampling, progressive, transfer_syntax in VARIANTS:
        with tempfile.TemporaryDirectory(prefix="wsi-dicom-12bit-") as temporary:
            frames, reference = encode_frames(Path(temporary), photometric, sampling, progressive)
        write_netpbm(
            args.output / (name + ".ppm"),
            MATRIX_WIDTH,
            MATRIX_HEIGHT,
            [tuple(reference[i:i + 3]) for i in range(0, len(reference), 3)],
        )
        make_dicom(name, photometric, transfer_syntax, frames, args.output / (name + ".dcm"))
        print(f"{name}: frames={len(frames)}, bytes={sum(map(len, frames))}")


if __name__ == "__main__":
    main()
