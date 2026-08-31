#!/usr/bin/env python3
"""Generate deterministic unsigned release metadata, checksums, and SBOMs."""

import argparse
import hashlib
import json
import pathlib
import re


def spdx_id(name: str, version: str) -> str:
    value = re.sub(r"[^A-Za-z0-9.-]", "-", f"{name}-{version}")
    return f"SPDXRef-Package-{value}"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=pathlib.Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--gpu-feature", choices=("metal", "cuda"), required=True)
    parser.add_argument("--cargo-metadata", type=pathlib.Path, required=True)
    parser.add_argument("--output-dir", type=pathlib.Path, required=True)
    args = parser.parse_args()

    artifact = args.artifact.resolve(strict=True)
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    cargo = json.loads(args.cargo_metadata.read_text(encoding="utf-8"))
    packages = sorted(cargo["packages"], key=lambda package: (package["name"], package["version"]))
    digest = hashlib.sha256(artifact.read_bytes()).hexdigest()

    release_metadata = {
        "schema_version": 1,
        "artifact": artifact.name,
        "target": args.target,
        "signed": False,
        "gpu_feature": args.gpu_feature,
        "gpu_runtime_required": False,
        "sha256": digest,
    }
    (output / "artifact-metadata.json").write_text(
        json.dumps(release_metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output / "SHA256SUMS").write_text(f"{digest}  {artifact.name}\n", encoding="utf-8")

    artifact_spdx = "SPDXRef-Package-wsi-rs-openslide-artifact"
    namespace_hash = hashlib.sha256(f"{digest}:{args.target}".encode()).hexdigest()
    spdx_packages = [{
        "SPDXID": artifact_spdx,
        "name": artifact.name,
        "versionInfo": args.target,
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": False,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "MIT OR Apache-2.0",
        "checksums": [{"algorithm": "SHA256", "checksumValue": digest}],
    }]
    spdx_packages.extend({
        "SPDXID": spdx_id(package["name"], package["version"]),
        "name": package["name"],
        "versionInfo": package["version"],
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": False,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": package.get("license") or "NOASSERTION",
    } for package in packages)
    spdx = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"wsi-rs-openslide-{args.target}",
        "documentNamespace": f"https://wsi-rs.invalid/spdx/{namespace_hash}",
        "creationInfo": {"creators": ["Tool: wsi-rs-release-metadata-1"], "created": "1970-01-01T00:00:00Z"},
        "packages": spdx_packages,
        "relationships": [{
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": artifact_spdx,
        }],
    }
    (output / "sbom.spdx.json").write_text(
        json.dumps(spdx, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    components = [{
        "type": "library",
        "name": package["name"],
        "version": package["version"],
        "purl": f"pkg:cargo/{package['name']}@{package['version']}",
        **({"licenses": [{"expression": package["license"]}]} if package.get("license") else {}),
    } for package in packages]
    cyclonedx = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{namespace_hash[:8]}-{namespace_hash[8:12]}-{namespace_hash[12:16]}-{namespace_hash[16:20]}-{namespace_hash[20:32]}",
        "version": 1,
        "metadata": {"component": {
            "type": "file",
            "name": artifact.name,
            "version": args.target,
            "hashes": [{"alg": "SHA-256", "content": digest}],
        }},
        "components": components,
    }
    (output / "sbom.cdx.json").write_text(
        json.dumps(cyclonedx, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
