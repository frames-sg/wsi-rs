#!/usr/bin/env python3
"""Generate deterministic, compact parser-reaching fuzz seeds.

The bundle fuzz targets deliberately write the same input bytes to every file.
Seeds therefore maximize useful parser depth under that contract rather than
pretending to be complete multi-file specimens.
"""

from __future__ import annotations

import json
import struct
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SEEDS = ROOT / "fuzz" / "seeds"
JP2K_FIXTURE = ROOT / "tests" / "fixtures" / "jp2k" / "rgb_nomct.j2k"


def write(target: str, name: str, data: bytes) -> None:
    directory = SEEDS / target
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_bytes(data)


def short(*values: int) -> bytes:
    return struct.pack(f"<{len(values)}H", *values)


def long(*values: int) -> bytes:
    return struct.pack(f"<{len(values)}I", *values)


def ascii_value(value: str) -> bytes:
    return value.encode("utf-8") + b"\0"


def classic_tiled_rgb(
    *,
    width: int = 17,
    height: int = 13,
    tile_width: int = 16,
    tile_height: int = 8,
    extra_tags: list[tuple[int, int, bytes]] | None = None,
) -> bytes:
    """Build a one-IFD little-endian TIFF with full physical edge tiles."""
    tiles_across = (width + tile_width - 1) // tile_width
    tiles_down = (height + tile_height - 1) // tile_height
    tile_count = tiles_across * tiles_down
    physical_tile_bytes = tile_width * tile_height * 3

    tags: list[tuple[int, int, bytes | None]] = [
        (256, 4, long(width)),
        (257, 4, long(height)),
        (258, 3, short(8, 8, 8)),
        (259, 3, short(1)),
        (262, 3, short(2)),
        (277, 3, short(3)),
        (284, 3, short(1)),
        (322, 4, long(tile_width)),
        (323, 4, long(tile_height)),
        (324, 4, None),
        (325, 4, long(*([physical_tile_bytes] * tile_count))),
        (339, 3, short(1, 1, 1)),
    ]
    if extra_tags:
        tags.extend(extra_tags)
    tags.sort(key=lambda entry: entry[0])

    ifd_size = 2 + 12 * len(tags) + 4
    out_of_line_start = 8 + ifd_size
    out_of_line_size = 0
    for tag, _tiff_type, value in tags:
        value_len = 4 * tile_count if tag == 324 else len(value or b"")
        if value_len > 4:
            out_of_line_size += value_len
            if out_of_line_size & 1:
                out_of_line_size += 1
    payload_start = out_of_line_start + out_of_line_size
    tile_offsets = [payload_start + index * physical_tile_bytes for index in range(tile_count)]

    header = bytearray(b"II" + short(42) + long(8))
    ifd = bytearray(short(len(tags)))
    out_of_line = bytearray()
    for tag, tiff_type, value in tags:
        if tag == 324:
            value = long(*tile_offsets)
        assert value is not None
        type_size = {1: 1, 2: 1, 3: 2, 4: 4, 7: 1}[tiff_type]
        assert len(value) % type_size == 0
        count = len(value) // type_size
        ifd.extend(short(tag, tiff_type))
        ifd.extend(long(count))
        if len(value) <= 4:
            ifd.extend(value.ljust(4, b"\0"))
        else:
            offset = out_of_line_start + len(out_of_line)
            ifd.extend(long(offset))
            out_of_line.extend(value)
            if len(out_of_line) & 1:
                out_of_line.append(0)
    ifd.extend(long(0))

    payload = bytearray()
    for tile_index in range(tile_count):
        for y in range(tile_height):
            for x in range(tile_width):
                payload.extend(
                    (
                        (x + tile_index * 17) & 0xFF,
                        (y + tile_index * 29) & 0xFF,
                        (x + y + tile_index * 43) & 0xFF,
                    )
                )
    result = bytes(header + ifd + out_of_line + payload)
    assert result[:8] == b"II*\0\x08\0\0\0"
    assert len(result) == payload_start + tile_count * physical_tile_bytes
    return result


def selector(index: int, payload: bytes) -> bytes:
    # open_wsi_bytes maps selector modulo 13 onto its extension table.
    assert 0 <= index < 13
    return bytes([index]) + payload


def explicit_vr_element(group: int, element: int, vr: bytes, value: bytes) -> bytes:
    if len(value) & 1:
        value += b"\0" if vr == b"UI" else b" "
    prefix = struct.pack("<HH", group, element) + vr
    if vr in {b"OB", b"OD", b"OF", b"OL", b"OW", b"SQ", b"UC", b"UR", b"UT", b"UN"}:
        return prefix + b"\0\0" + long(len(value)) + value
    return prefix + short(len(value)) + value


def dicom_wsi() -> bytes:
    sop_class = b"1.2.840.10008.5.1.4.1.1.77.1.6"
    sop_instance = b"1.2.826.0.1.3680043.10.777.1"
    series_instance = b"1.2.826.0.1.3680043.10.777"
    transfer_syntax = b"1.2.840.10008.1.2.1"
    implementation = b"1.2.826.0.1.3680043.10.777.99"

    meta_body = b"".join(
        [
            explicit_vr_element(0x0002, 0x0001, b"OB", b"\0\x01"),
            explicit_vr_element(0x0002, 0x0002, b"UI", sop_class),
            explicit_vr_element(0x0002, 0x0003, b"UI", sop_instance),
            explicit_vr_element(0x0002, 0x0010, b"UI", transfer_syntax),
            explicit_vr_element(0x0002, 0x0012, b"UI", implementation),
        ]
    )
    meta = explicit_vr_element(0x0002, 0x0000, b"UL", long(len(meta_body))) + meta_body
    dataset = b"".join(
        [
            explicit_vr_element(0x0008, 0x0008, b"CS", b"ORIGINAL\\PRIMARY\\VOLUME\\RESAMPLED"),
            explicit_vr_element(0x0008, 0x0016, b"UI", sop_class),
            explicit_vr_element(0x0008, 0x0018, b"UI", sop_instance),
            explicit_vr_element(0x0020, 0x000E, b"UI", series_instance),
            explicit_vr_element(0x0028, 0x0002, b"US", short(3)),
            explicit_vr_element(0x0028, 0x0004, b"CS", b"RGB"),
            explicit_vr_element(0x0028, 0x0006, b"US", short(0)),
            explicit_vr_element(0x0028, 0x0008, b"IS", b"1"),
            explicit_vr_element(0x0028, 0x0010, b"US", short(2)),
            explicit_vr_element(0x0028, 0x0011, b"US", short(2)),
            explicit_vr_element(0x0028, 0x0030, b"DS", b"0.00025\\0.00025"),
            explicit_vr_element(0x0028, 0x0100, b"US", short(8)),
            explicit_vr_element(0x0028, 0x0101, b"US", short(8)),
            explicit_vr_element(0x0028, 0x0102, b"US", short(7)),
            explicit_vr_element(0x0028, 0x0103, b"US", short(0)),
            explicit_vr_element(0x0048, 0x0006, b"UL", long(2)),
            explicit_vr_element(0x0048, 0x0007, b"UL", long(2)),
            explicit_vr_element(0x7FE0, 0x0010, b"OB", bytes(range(12))),
        ]
    )
    result = bytes(128) + b"DICM" + meta + dataset
    assert result[128:132] == b"DICM"
    return result


def ets_scene(jp2k: bytes) -> bytes:
    additional_header_offset = 64
    chunk_table_offset = 256
    n_dimensions = 3
    entry_len = 20 + n_dimensions * 4
    payload_offset = chunk_table_offset + entry_len

    data = bytearray()
    data.extend(b"SIS\0")
    data.extend(long(48, 1, n_dimensions))
    data.extend(struct.pack("<Q", additional_header_offset))
    data.extend(long(156, 0))
    data.extend(struct.pack("<Q", chunk_table_offset))
    data.extend(long(1, 0))
    data.extend(bytes(additional_header_offset - len(data)))
    data.extend(b"ETS\0")
    data.extend(long(0, 1, 3, 0, 3, 100, 16, 12, 1))
    data.extend(long(*([0] * 17)))
    data.extend(bytes((7, 11, 13)))
    data.extend(bytes(additional_header_offset + 108 + 40 - len(data)))
    data.extend(long(0, 0))
    data.extend(bytes(chunk_table_offset - len(data)))
    data.extend(long(0))
    data.extend(struct.pack("<iii", 1, 0, 0))
    data.extend(struct.pack("<Q", payload_offset))
    data.extend(long(len(jp2k), 0))
    assert len(data) == payload_offset
    data.extend(jp2k)
    return bytes(data)


def svcache() -> bytes:
    metadata = {
        "schema_version": 4,
        "complete": True,
        "source": {
            "path": "seed",
            "len": 0,
            "modified_unix_nanos": None,
            "sha256": "0" * 64,
        },
        "properties": [["openslide.vendor", "svcache"]],
        "scenes": [],
        "associated": [],
    }
    encoded = json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode("ascii")
    return b"SVCACHE1" + struct.pack("<Q", len(encoded)) + encoded


def cfb_header() -> bytes:
    # A complete 512-byte CFB header with no sectors. This reaches the compound
    # parser and fails closed when it attempts to resolve the empty directory.
    data = bytearray(512)
    data[:8] = bytes.fromhex("d0cf11e0a1b11ae1")
    data[24:26] = short(0x003E)
    data[26:28] = short(0x0003)
    data[28:30] = short(0xFFFE)
    data[30:32] = short(9)
    data[32:34] = short(6)
    data[40:44] = long(0)
    data[44:48] = long(0)
    data[48:52] = long(0xFFFFFFFE)
    data[56:60] = long(4096)
    data[60:64] = long(0xFFFFFFFE)
    data[64:68] = long(0)
    data[68:72] = long(0xFFFFFFFE)
    data[72:76] = long(0)
    for offset in range(76, 512, 4):
        data[offset : offset + 4] = long(0xFFFFFFFF)
    return bytes(data)


def main() -> None:
    jp2k = JP2K_FIXTURE.read_bytes()
    assert jp2k.startswith(b"\xffO")

    partial_edge = classic_tiled_rgb()
    aperio = classic_tiled_rgb(
        extra_tags=[(270, 2, ascii_value("Aperio Image Library v12.4.0\nMPP = 0.25"))]
    )
    argos = classic_tiled_rgb(
        extra_tags=[
            (
                65000,
                2,
                ascii_value(
                    "<Argos.Scan.Metadata><MinZ>0</MinZ><MaxZ>0</MaxZ>"
                    "<ObjectiveMagnification>20</ObjectiveMagnification>"
                    "<Barcode>FUZZ-SEED</Barcode></Argos.Scan.Metadata>"
                ),
            )
        ]
    )
    huron = classic_tiled_rgb(
        extra_tags=[
            (270, 2, ascii_value("Scanner = LE176\nObjective = 20\nMPP = 0.25")),
            (271, 2, ascii_value("Huron LE176")),
        ]
    )
    leica = classic_tiled_rgb(
        extra_tags=[
            (
                270,
                2,
                ascii_value('<scn><collection sizeX="17" sizeY="13"></collection></scn>'),
            )
        ]
    )
    philips = classic_tiled_rgb(
        extra_tags=[
            (270, 2, ascii_value('<DataObject ObjectType="DPUfsImport"></DataObject>')),
            (305, 2, ascii_value("Philips UFS")),
        ]
    )
    trestle = classic_tiled_rgb(
        extra_tags=[
            (270, 2, ascii_value("OverlapsXY=1 1")),
            (305, 2, ascii_value("MedScan 1.0")),
        ]
    )
    ventana_xml = (
        '<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" NumCols="11" '
        'NumRows="-17" Pos-X="0" Pos-Y="0"/></SlideStitchInfo></EncodeInfo>'
    )
    ventana_exploit = classic_tiled_rgb(
        extra_tags=[
            (270, 2, ascii_value("level=0;" + ventana_xml)),
            (700, 2, ascii_value("<xmp><iScan/></xmp>")),
        ]
    )
    ndpi = classic_tiled_rgb()
    dicom = dicom_wsi()
    vsi = ets_scene(jp2k)
    zvi = cfb_header()

    vms_ini = (
        "[Virtual Microscope Specimen]\n"
        "NoJpegColumns=1\nNoJpegRows=1\n"
        "ImageFile(0,0)=image0.jpg\n"
        "MapFile=map.jpg\nMacroImage=macro.jpg\n"
        "OptimisationFile=optimisation.bin\n"
        "SourceLens=20\nPhysicalWidth=16\nPhysicalHeight=8\n"
    ).encode("ascii")
    mirax_ini = (
        "[GENERAL]\nSLIDE_ID=FUZZ\nIMAGENUMBER_X=1\nIMAGENUMBER_Y=1\n"
        "OBJECTIVE_MAGNIFICATION=20x\nCameraImageDivisionsPerSide=1\n"
        "[HIERARCHICAL]\nINDEXFILE=Index.dat\nHIER_COUNT=1\nNONHIER_COUNT=0\n"
        "HIER_0_NAME=Slide zoom level\nHIER_0_COUNT=1\n"
        "HIER_0_VAL_0_SECTION=LEVEL0\n"
        "[DATAFILE]\nFILE_COUNT=1\nFILE_0=Data0.dat\n"
        "[LEVEL0]\nIMAGE_CONCAT_FACTOR=0\nDIGITIZER_WIDTH=16\n"
        "DIGITIZER_HEIGHT=16\nIMAGE_FILL_COLOR_BGR=0\n"
        "MICROMETER_PER_PIXEL_X=0.25\nMICROMETER_PER_PIXEL_Y=0.25\n"
        "IMAGE_FORMAT=JPEG\nOVERLAP_X=0\nOVERLAP_Y=0\n"
    ).encode("ascii")

    # Dedicated backend targets.
    write("open_jp2k_codestream_bytes", "rgb_nomct.j2k", jp2k)
    write("open_dicom_bytes", "native_rgb_wsi.dcm", dicom)
    write("open_svcache_bytes", "schema4-empty.svcache", svcache())
    write("open_vsi_bundle_bytes", "ets-jp2k-scene.bin", vsi)
    write("open_vms_bundle_bytes", "vms-key.ini", vms_ini)
    write("open_mirax_bundle_bytes", "mirax-slidedat.ini", mirax_ini)
    write("open_zvi_bytes", "empty-cfb.zvi", zvi)

    # TIFF interpreter and extension routing through open_wsi_bytes.
    write("open_wsi_bytes", "generic-partial-edge.tif", selector(4, partial_edge))
    write("open_wsi_bytes", "aperio.svs", selector(0, aperio))
    write("open_wsi_bytes", "argos.avs", selector(1, argos))
    write("open_wsi_bytes", "ndpi-container.ndpi", selector(2, ndpi))
    write("open_wsi_bytes", "leica.scn", selector(3, leica))
    write("open_wsi_bytes", "huron.tiff", selector(5, huron))
    write("open_wsi_bytes", "ventana-cve-2026-48977.bif", selector(6, ventana_exploit))
    write("open_wsi_bytes", "philips.tif", selector(4, philips))
    write("open_wsi_bytes", "trestle.tif", selector(4, trestle))
    write("open_wsi_bytes", "mirax-route.mrxs", selector(7, mirax_ini))
    write("open_wsi_bytes", "vms-route.vms", selector(8, vms_ini))
    write("open_wsi_bytes", "vmu-route.vmu", selector(9, vms_ini))
    write("open_wsi_bytes", "vsi-route.vsi", selector(10, vsi))
    write("open_wsi_bytes", "dicom-route.dcm", selector(11, dicom))
    write("open_wsi_bytes", "zvi-route.zvi", selector(12, zvi))

    # Shared XML parser seeds include both newly added and exploit-class XML.
    write(
        "parse_xml_bytes",
        "argos-metadata.xml",
        b"<Argos.Scan.Metadata><MinZ>0</MinZ><MaxZ>0</MaxZ></Argos.Scan.Metadata>",
    )
    write("parse_xml_bytes", "ventana-negative-grid.xml", ventana_xml.encode("ascii"))
    write(
        "parse_xml_bytes",
        "leica-collection.xml",
        b'<scn><collection sizeX="17" sizeY="13"><image/></collection></scn>',
    )

if __name__ == "__main__":
    main()
