# Dependency policy

`cargo xtask deps` checks every dependency before a release. It runs, with
locked versions:

- [cargo-deny](https://embarkstudios.github.io/cargo-deny/): known
  vulnerabilities, licenses and allowed sources.
- cargo-machete: unused dependencies.
- [cargo-vet](https://mozilla.github.io/cargo-vet/): every dependency must be
  audited.

CI installs pinned versions of all three tools. Git dependencies are limited
to the first-party repositories listed in `deny.toml`; see
[First-party Git dependencies](#first-party-git-dependencies).

## Audits

`supply-chain/` holds the cargo-vet data. Most audits are imported from the
Bytecode Alliance, Google and Mozilla. A crate nobody has audited needs an
explicit `safe-to-deploy` exemption. Any dependency change must pass
`cargo vet --locked`, and any new exemption is reviewed in the same change.

Some dependencies are reviewed here for one exact version. Upgrading any of
them needs a new review:

- **Python binding dependencies**: exact-version audits cover PyO3 and its
  four support packages at 0.30.1, `portable-atomic 1.15.0`,
  `target-lexicon 0.13.5`, and the `syn 2.0.119 -> 3.0.9` delta. The notes
  describe the stable-ABI reader integration and review limits.
- **Zstandard**: delta audits cover `zstd 0.14.3`, `zstd-safe 8.2.0`, and
  `zstd-sys 2.1.2+zstd.1.5.7`. CZI parsing and its tests use the native
  reader; there is no external CZI reader dependency.
- **`sha2-asm` 0.6.4**: on Apple Silicon Macs, `sha2` uses its `asm` feature so
  slide fingerprints (dataset IDs and `.svcache` checks) use the CPU's SHA-256
  instructions. The audit in `supply-chain/audits.toml` covers the package's
  build script and the assembly it compiles. Tests check that the hardware and
  software paths give the same results. Building on macOS needs the system C
  toolchain.

## First-party Git dependencies

J2K (`frames-sg/j2k`) and JXR (`frames-sg/jxr`) are first-party code, followed on
their `main` branches. `cargo latest`, an alias in `.cargo/config.toml`, moves
`Cargo.lock` to their newest commits, and every CI job runs it before building
with `--locked`, so every other dependency stays pinned. JXR's `main` builds on
J2K `main`, so a slide reader compiles one J2K.

cargo-vet treats every J2K and JXR package as first-party
(`audit-as-crates-io = false`). They are reviewed in their own repositories, not
audited per revision here. The audits of earlier exact J2K revisions, the
published JXR audits, and the violation against the published CUDA engine 0.13.0
stay in `supply-chain/audits.toml` and apply to those crates.io packages. Release
artifacts record the exact J2K and JXR commits in their SBOMs.

## Exceptions with an expiry date

| Dependency | Why it is allowed | Owner | Review by |
| --- | --- | --- | --- |
| `encoding 0.2.33` | Unmaintained, pulled in by `dicom-encoding 0.9.1`. The DICOM parsers bound all text they decode, and cargo-deny still rejects any known RustSec vulnerability. dicom-rs 0.10.0 still depends on it (checked 2026-10-01). | wsi-rs maintainers | 2027-01-01 or the next dicom-rs release, whichever comes first |
