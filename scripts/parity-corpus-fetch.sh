#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'USAGE'
usage: scripts/parity-corpus-fetch.sh [alias ...]

Downloads public parity-corpus slides from tests/fixtures/parity_corpus.public.toml.
Files are written to $WSI_RS_PARITY_CORPUS_CACHE, or to
~/.cache/slideviewer/parity-corpus when the variable is unset.

Pass one or more aliases such as svs-001 or dicom-jp2k-001 to fetch only those
slides. Set WSI_RS_PARITY_CORPUS_FORCE=1 to re-extract zip archives.
USAGE
  exit 0
fi

python3 - "$repo_root" "$@" <<'PY'
from __future__ import annotations

import hashlib
import os
import shutil
import stat
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path
from pathlib import PurePosixPath

try:
    import tomllib
except ModuleNotFoundError as exc:
    raise SystemExit("python 3.11+ with tomllib is required") from exc

FORMAT_EXTENSIONS = {
    "aperio": "svs",
    "leica": "scn",
    "ventana": "bif",
    "philips_tiff": "tif",
    "tiff": "tif",
    "ndpi": "ndpi",
    "hamamatsu_vms": "zip",
    "dicom": "dcm",
    "mirax": "zip",
}
MAX_ZIP_MEMBERS = 50_000
MAX_ZIP_MEMBER_BYTES = 2 * 1024**3
MAX_ZIP_EXPANDED_BYTES = 16 * 1024**3
MAX_ZIP_COMPRESSION_RATIO = 1_000


def main() -> int:
    repo_root = Path(sys.argv[1])
    aliases = {arg for arg in sys.argv[2:] if arg}
    manifest_path = repo_root / "tests" / "fixtures" / "parity_corpus.public.toml"
    with manifest_path.open("rb") as handle:
        manifest = tomllib.load(handle)

    cache_dir = Path(
        os.environ.get(
            "WSI_RS_PARITY_CORPUS_CACHE",
            Path.home() / ".cache" / "slideviewer" / "parity-corpus",
        )
    ).expanduser()
    cache_dir.mkdir(parents=True, exist_ok=True)
    force_extract = truthy(os.environ.get("WSI_RS_PARITY_CORPUS_FORCE"))

    slides = manifest.get("slide", [])
    selected = [
        entry for entry in slides if not aliases or entry.get("alias") in aliases
    ]
    if aliases:
        found = {entry.get("alias") for entry in selected}
        missing = sorted(aliases - found)
        if missing:
            raise SystemExit(f"unknown corpus aliases: {', '.join(missing)}")

    for entry in selected:
        fetch_entry(entry, cache_dir, force_extract)
    return 0


def fetch_entry(entry: dict[str, object], cache_dir: Path, force_extract: bool) -> None:
    alias = str(entry["alias"])
    url = str(entry.get("url", ""))
    expected_sha = str(entry.get("sha256", ""))
    if not url:
        print(f"[skip] {alias}: no URL")
        return

    target = target_path(entry, cache_dir)
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.is_file() and (not expected_sha or sha256_file(target) == expected_sha):
        print(f"[ok]   {alias}: {target}")
    else:
        download(url, target)
        if expected_sha:
            actual_sha = sha256_file(target)
            if actual_sha != expected_sha:
                target.unlink(missing_ok=True)
                raise SystemExit(
                    f"{alias}: sha256 mismatch for {target}: "
                    f"expected {expected_sha}, got {actual_sha}"
                )
        print(f"[get]  {alias}: {target}")

    if target.suffix.lower() == ".zip":
        extract_zip(alias, target, cache_dir / f"{alias}.d", force_extract)


def target_path(entry: dict[str, object], cache_dir: Path) -> Path:
    alias = str(entry["alias"])
    url = str(entry.get("url", ""))
    url_name = url.rsplit("/", 1)[-1]
    url_suffix = Path(url_name).suffix.lower()
    if url_suffix == ".zip":
        return cache_dir / f"{alias}.zip"

    ext = FORMAT_EXTENSIONS.get(str(entry.get("format", "")))
    if not ext and url_suffix:
        ext = url_suffix.lstrip(".")
    return cache_dir / (f"{alias}.{ext}" if ext else alias)


def download(url: str, target: Path) -> None:
    fd, tmp_name = tempfile.mkstemp(prefix=f".{target.name}.", dir=target.parent)
    os.close(fd)
    tmp_path = Path(tmp_name)
    try:
        with urllib.request.urlopen(url) as response, tmp_path.open("wb") as handle:
            shutil.copyfileobj(response, handle)
        tmp_path.replace(target)
    except Exception:
        tmp_path.unlink(missing_ok=True)
        raise


def extract_zip(alias: str, archive: Path, extract_dir: Path, force_extract: bool) -> None:
    if extract_dir.is_dir() and not force_extract:
        print(f"[ok]   {alias}: {extract_dir}")
        return

    tmp_dir = extract_dir.with_name(f".{extract_dir.name}.tmp")
    if tmp_dir.exists():
        shutil.rmtree(tmp_dir)
    if extract_dir.exists():
        shutil.rmtree(extract_dir)
    tmp_dir.mkdir(parents=True)
    try:
        with zipfile.ZipFile(archive) as zf:
            members = validated_zip_members(alias, zf)
            for info, relative in members:
                target = tmp_dir.joinpath(*relative.parts)
                if info.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                with zf.open(info) as source, target.open("xb") as destination:
                    copy_zip_member(alias, info, source, destination)
        tmp_dir.replace(extract_dir)
    except Exception:
        shutil.rmtree(tmp_dir, ignore_errors=True)
        raise
    print(f"[ext]  {alias}: {extract_dir}")


def validated_zip_members(
    alias: str, zf: zipfile.ZipFile
) -> list[tuple[zipfile.ZipInfo, PurePosixPath]]:
    infos = zf.infolist()
    if len(infos) > MAX_ZIP_MEMBERS:
        raise ValueError(f"{alias}: archive has too many members: {len(infos)}")

    expanded = 0
    seen: set[PurePosixPath] = set()
    members: list[tuple[zipfile.ZipInfo, PurePosixPath]] = []
    for info in infos:
        relative = PurePosixPath(info.filename)
        if (
            not info.filename
            or "\\" in info.filename
            or relative.is_absolute()
            or any(part in {"", ".", ".."} for part in relative.parts)
        ):
            raise ValueError(f"{alias}: unsafe archive member: {info.filename!r}")
        if relative in seen:
            raise ValueError(f"{alias}: duplicate archive member: {info.filename!r}")
        seen.add(relative)

        unix_mode = info.external_attr >> 16
        file_type = unix_mode & 0o170000
        if file_type not in {0, 0o040000, 0o100000}:
            raise ValueError(f"{alias}: link or special archive member: {info.filename!r}")
        if info.flag_bits & 0x1:
            raise ValueError(f"{alias}: encrypted archive member: {info.filename!r}")
        if info.file_size > MAX_ZIP_MEMBER_BYTES:
            raise ValueError(f"{alias}: archive member is too large: {info.filename!r}")
        expanded += info.file_size
        if expanded > MAX_ZIP_EXPANDED_BYTES:
            raise ValueError(f"{alias}: archive expanded size exceeds limit")
        if (
            info.file_size > 0
            and info.compress_size == 0
            or info.compress_size > 0
            and info.file_size / info.compress_size > MAX_ZIP_COMPRESSION_RATIO
        ):
            raise ValueError(f"{alias}: archive member compression ratio exceeds limit")
        members.append((info, relative))
    return members


def copy_zip_member(alias: str, info, source, destination) -> None:
    remaining = info.file_size
    while remaining:
        chunk = source.read(min(1024 * 1024, remaining))
        if not chunk:
            raise ValueError(f"{alias}: truncated archive member: {info.filename!r}")
        destination.write(chunk)
        remaining -= len(chunk)
    if source.read(1):
        raise ValueError(f"{alias}: archive member exceeds declared size: {info.filename!r}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def truthy(value: str | None) -> bool:
    return value is not None and value.lower() in {"1", "true", "yes", "on"}


if truthy(os.environ.get("WSI_RS_PARITY_CORPUS_SELF_TEST")):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        archive = root / "test.zip"
        with zipfile.ZipFile(archive, "w") as zf:
            zf.writestr("safe/file.txt", b"ok")
        extract_zip("self-test", archive, root / "out", True)
        assert (root / "out/safe/file.txt").read_bytes() == b"ok"

        with zipfile.ZipFile(archive, "w") as zf:
            zf.writestr("../escape", b"bad")
        try:
            extract_zip("self-test", archive, root / "bad", True)
        except ValueError:
            pass
        else:
            raise AssertionError("parent traversal archive was accepted")

        class FakeZip:
            def __init__(self, infos):
                self._infos = infos

            def infolist(self):
                return self._infos

        def rejected(infos) -> None:
            try:
                validated_zip_members("self-test", FakeZip(infos))
            except ValueError:
                return
            raise AssertionError("unsafe archive member list was accepted")

        rejected([zipfile.ZipInfo("/absolute")])
        rejected([zipfile.ZipInfo("duplicate"), zipfile.ZipInfo("duplicate")])
        symlink_info = zipfile.ZipInfo("link")
        symlink_info.external_attr = (stat.S_IFLNK | 0o777) << 16
        rejected([symlink_info])
        large_info = zipfile.ZipInfo("large")
        large_info.file_size = MAX_ZIP_MEMBER_BYTES + 1
        rejected([large_info])
        ratio_info = zipfile.ZipInfo("bomb")
        ratio_info.file_size = MAX_ZIP_COMPRESSION_RATIO + 1
        ratio_info.compress_size = 1
        rejected([ratio_info])
else:
    raise SystemExit(main())
PY
