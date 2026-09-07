#!/usr/bin/env python3
"""Derive deidentified, single-frame codec fixtures from public OpenSlide data.

Run with uv run --with pydicom==3.0.2 --with pillow==12.1.1
scripts/prepare-public-dicom-fixtures.py. Requires opj_decompress 2.5.4.
The source ZIPs belong in ~/.cache/slideviewer/parity-corpus. DICOM compressed
frames are not re-encoded. Their metadata is synthetic; in particular,
the 3DHISTECH source's incorrect baseline transfer syntax is corrected to JPEG
Full Progression. Reference pixels come from libjpeg/Pillow or OpenJPEG, never
from J2K or wsi-rs. A complete SVCACHE is also built with Cargo from the
deidentified RCT fixture. Sources are CC0 OpenSlide testdata.
"""

import argparse
import hashlib
import io
from pathlib import Path
import subprocess
import shutil
import tempfile
import uuid
import zipfile

from PIL import Image
from pydicom.dataset import FileDataset, FileMetaDataset
from pydicom.encaps import encapsulate, generate_frames
from pydicom import dcmread
from pydicom.uid import VLWholeSlideMicroscopyImageStorage


SOURCES = (
    ("progressive-sof2", "dicom-progressive-001.zip", "000005.dcm",
     "f7306843363c08ab86539f52acbe492917c440df50231d06b9b2e1657ac9e6a8",
     "1.2.840.10008.1.2.4.55", "jpeg"),
    ("ybr-rct", "dicom-rct-001.zip", "DCM_0.dcm",
     "7e919f6bfaf0424f9fb5292e44d32e0c66cae6a43a8aa309a7eca9c44e0c2d1d",
     "1.2.840.10008.1.2.4.90", "j2k"),
    ("ybr-ict", "dicom-ict-001.zip", "img_0.dcm",
     "d07593742d5bf1b0f3de3937c110c95c2480b8a525b398336e493bac3689497d",
     "1.2.840.10008.1.2.4.91", "j2k"),
)


def uid(name):
    return "2.25." + str(uuid.uuid5(uuid.NAMESPACE_URL, "wsi-rs/public-fixture/" + name).int)


def make_fixture(name, header, frame, transfer_syntax, output):
    meta = FileMetaDataset()
    meta.TransferSyntaxUID = transfer_syntax
    meta.MediaStorageSOPClassUID = VLWholeSlideMicroscopyImageStorage
    meta.MediaStorageSOPInstanceUID = uid(name)
    dataset = FileDataset(None, {}, file_meta=meta, preamble=bytes(128))
    # Build from an allowlist instead of copying patient, specimen, institution,
    # date, private, or per-frame metadata out of the source study.
    for field in ("Rows", "Columns", "SamplesPerPixel", "PhotometricInterpretation",
                  "BitsAllocated", "BitsStored", "HighBit", "PixelRepresentation"):
        setattr(dataset, field, getattr(header, field))
    dataset.PlanarConfiguration = 0
    dataset.SOPClassUID = VLWholeSlideMicroscopyImageStorage
    dataset.SOPInstanceUID = uid(name)
    dataset.StudyInstanceUID = uid("study")
    dataset.SeriesInstanceUID = uid(name + "/series")
    dataset.FrameOfReferenceUID = uid(name + "/frame")
    dataset.Modality = "SM"
    dataset.PatientName = "PUBLIC^FIXTURE"
    dataset.PatientID = "PUBLIC-FIXTURE"
    dataset.PatientIdentityRemoved = "YES"
    dataset.DeidentificationMethod = "Synthetic allowlisted metadata; public tissue frame only"
    dataset.ImageType = ["DERIVED", "PRIMARY", "VOLUME", "NONE"]
    dataset.DimensionOrganizationType = "TILED_FULL"
    dataset.NumberOfFrames = 1
    dataset.TotalPixelMatrixRows = header.Rows
    dataset.TotalPixelMatrixColumns = header.Columns
    dataset.TotalPixelMatrixFocalPlanes = 1
    dataset.NumberOfOpticalPaths = 1
    dataset.PixelSpacing = [1, 1]  # Synthetic geometry: these are codec fixtures.
    dataset.PixelData = encapsulate([frame])
    dataset[0x7FE00010].is_undefined_length = True
    destination = output / (name + ".dcm")
    dataset.save_as(destination, enforce_file_format=True)
    recovered = dcmread(destination)
    assert next(generate_frames(recovered.PixelData, number_of_frames=1)) == frame


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path,
                        default=Path.home() / ".cache/slideviewer/parity-corpus")
    parser.add_argument("--output", type=Path,
                        default=Path(__file__).resolve().parents[1] / "tests/fixtures/public_dicom")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for name, archive_name, member, digest, transfer_syntax, codec in SOURCES:
        archive = args.cache / archive_name
        with archive.open("rb") as source:
            if hashlib.file_digest(source, "sha256").hexdigest() != digest:
                raise ValueError("source archive differs from OpenSlide's published digest: " + archive_name)
        with zipfile.ZipFile(archive) as container, container.open(member) as source:
            header = dcmread(source, stop_before_pixels=True)
            pixel_header = source.read(12)
            if pixel_header[:4] != b"\xe0\x7f\x10\x00":
                raise ValueError("expected explicit-VR encapsulated Pixel Data")
            frames = generate_frames(source, number_of_frames=int(header.NumberOfFrames))
            if codec == "jpeg":
                # The scanner mixed baseline and progressive frames in this
                # object. Its progressive frames are background tiles. This
                # fixture covers that real encoded process/container boundary;
                # the existing generated JPEG tests cover varied-color scans.
                for frame_index, frame in enumerate(frames):
                    candidate = Image.open(io.BytesIO(frame))
                    if candidate.info.get("progressive"):
                        break
                else:
                    raise ValueError("no progressive frame in the source object")
            else:
                # The matrix center lies between tissue pieces. A substantial
                # compressed tile selects tissue; the independent decode below
                # separately verifies that the pixels are actually varied.
                frame_index, frame = next(
                    (index, value) for index, value in enumerate(frames) if len(value) > 65536
                )
        with tempfile.TemporaryDirectory(prefix="wsi-dicom-reference-") as temporary:
            if codec == "jpeg":
                reference = Image.open(io.BytesIO(frame))
                if not reference.info.get("progressive"):
                    raise ValueError("the real JPEG fixture must contain SOF2")
                reference = reference.convert("RGB")
            else:
                encoded = Path(temporary) / "frame.j2k"
                decoded = Path(temporary) / "frame.ppm"
                encoded.write_bytes(frame)
                subprocess.run(["opj_decompress", "-i", str(encoded), "-o", str(decoded)], check=True)
                reference = Image.open(decoded).convert("RGB")
            if codec != "jpeg" and len(set(reference.tobytes())) < 32:
                raise ValueError("reference frame lacks enough pixel variation: " + name)
            reference.save(args.output / (name + ".ppm"))
        make_fixture(name, header, frame, transfer_syntax, args.output)
        print(f"{name}: source={archive_name}/{member}, frame={frame_index}, bytes={len(frame)}")

    # A complete cache must remain readable after its source is gone. Use a
    # temporary, deidentified source; retained source identity is diagnostic
    # metadata, not a runtime dependency of this complete container.
    with tempfile.TemporaryDirectory(prefix="wsi-public-fixture-", dir="/tmp" if Path("/tmp").is_dir() else None) as temporary:
        source = Path(temporary) / "ybr-rct.dcm"
        shutil.copyfile(args.output / "ybr-rct.dcm", source)
        subprocess.run([
            "cargo", "run", "--locked", "--bin", "svcache", "--", "build", str(source),
            "--out", str((args.output / "ybr-rct.svcache").resolve()),
        ], cwd=Path(__file__).resolve().parents[1], check=True)


if __name__ == "__main__":
    main()
