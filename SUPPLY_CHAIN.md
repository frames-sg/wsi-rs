# Dependency policy

`cargo xtask deps` checks every dependency before a release. It runs, with
locked versions:

- [cargo-deny](https://embarkstudios.github.io/cargo-deny/): known
  vulnerabilities, licenses and allowed sources.
- cargo-machete: unused dependencies.
- [cargo-vet](https://mozilla.github.io/cargo-vet/): every dependency must be
  audited.

CI installs pinned versions of all three tools.

## Audits

`supply-chain/` holds the cargo-vet data. Most audits are imported from the
Bytecode Alliance, Google and Mozilla. A crate nobody has audited needs an
explicit `safe-to-deploy` exemption. Any dependency change must pass
`cargo vet --locked`, and any new exemption is reviewed in the same change.

Some dependencies are reviewed here for one exact version. Upgrading any of
them needs a new review:

- **J2K 0.11.3** (JPEG and JPEG 2000 codecs): exemptions cover its `objc2`,
  SIMD and CUDA engine dependencies.
- **JXR** (`jxr`, `jxr-core` and `jxr-native` 0.2.1, `jxr-math` 0.1.0; JPEG XR
  codec, source at https://github.com/frames-sg/jxr): audited in
  `supply-chain/audits.toml`. The audit covers the CPU decoder only.
- **`sha2-asm` 0.6.4**: on Apple Silicon Macs, `sha2` uses its `asm` feature so
  slide fingerprints (dataset IDs and `.svcache` checks) use the CPU's SHA-256
  instructions. The audit in `supply-chain/audits.toml` covers the package's
  build script and the assembly it compiles. Tests check that the hardware and
  software paths give the same results. Building on macOS needs the system C
  toolchain.

## Exceptions with an expiry date

| Dependency | Why it is allowed | Owner | Review by |
| --- | --- | --- | --- |
| `encoding 0.2.33` | Unmaintained, pulled in by `dicom-encoding 0.9.1`. The DICOM parsers bound all text they decode, and cargo-deny still rejects any known RustSec vulnerability. dicom-rs 0.10.0 still depends on it (checked 2026-10-01). | wsi-rs maintainers | 2027-01-01 or the next dicom-rs release, whichever comes first |
