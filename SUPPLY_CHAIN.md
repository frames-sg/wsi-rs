# Dependency Policy

`cargo xtask deps` checks every dependency with locked versions:

- [cargo-deny](https://embarkstudios.github.io/cargo-deny/): known vulnerabilities, licenses and
  allowed sources.
- cargo-machete: unused dependencies.
- [cargo-vet](https://mozilla.github.io/cargo-vet/): every dependency must be audited or exempted.

CI installs pinned versions of all three tools. Git dependencies are limited to the first-party
repositories listed in `deny.toml`.

## Audits

`supply-chain/audits.toml` records local source reviews and the violation for the published
`j2k-cuda-j2k-engine 0.13.0` CUDA data race. cargo-vet also imports audits from the Bytecode
Alliance, Google and Mozilla. Versions without a complete audit path need an explicit exemption
in `supply-chain/config.toml`. Any dependency change must pass `cargo vet --locked`, and a new
exemption is reviewed in the same change.

On Apple Silicon, `sha2` uses its `asm` feature (locally audited `sha2-asm`) so slide fingerprints
(dataset IDs and `.svcache` checks) use the CPU's SHA-256 instructions. Tests check that the
hardware and software paths agree. Building on macOS needs the system C toolchain.

## Git Dependencies

J2K ([frames-sg/j2k](https://github.com/frames-sg/j2k)) and JXR
([frames-sg/jxr](https://github.com/frames-sg/jxr)) are first-party. `Cargo.toml` names each with a
crates.io version and its Git `main` branch: builds from this repository use `main`, and the
published crate resolves the crates.io release. JXR's `main` builds on J2K's `main`, so a build
compiles one J2K.

`cargo latest`, an alias in `.cargo/config.toml`, moves `Cargo.lock` to their newest commits and
leaves every other dependency pinned. CI runs it before building with `--locked`, except the
package, publish and Python wheel jobs, which build from the committed `Cargo.lock`. The C
artifact job runs `cargo latest` before compiling the library. To identify the Git revisions in
a released C library, use its accompanying SBOM. SPDX records each revision in the package's
`downloadLocation`, and CycloneDX records it in the package URL's `vcs_url`.

cargo-vet treats J2K and JXR packages as first-party (`audit-as-crates-io = false`); they are
reviewed in their own repositories.

The DICOM crates come from [frames-sg/dicom-rs](https://github.com/frames-sg/dicom-rs), branch
`frames/core-0.9.1`: dicom-rs 0.9 with its date and time types moved from Chrono, which is
unmaintained, to Jiff. wsi-rs uses no DICOM date or time API, so it builds against the fork and the
crates.io release alike. The published crate resolves the crates.io release, which still depends on
Chrono. `cargo latest` leaves the fork at its locked commit. cargo-vet checks the fork's packages as
crates.io packages, exempted at the locked commit like the crates.io release before them.

## Exceptions With an Expiry Date

| Dependency        | Why it is allowed                                                                                                                                                                                                        | Owner              | Review by                                                      |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------ | -------------------------------------------------------------- |
| `encoding 0.2.33` | Unmaintained, pulled in by `dicom-encoding 0.9.1`. The DICOM parsers bound all text they decode, and cargo-deny still rejects any known RustSec vulnerability. dicom-rs 0.10.1 still depends on it (checked 2026-11-20). | wsi-rs maintainers | 2027-01-01 or the next dicom-rs release, whichever comes first |
