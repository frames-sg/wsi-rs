#!/usr/bin/env bash
set -euo pipefail

readonly BASELINE_VERSION="0.4.0"
readonly BASELINE_SHA256="947b5b1b7703f1f99eb2a0bf124373dae81024954fc29743231a8bbcdcad457c"
readonly USER_AGENT="wsi-rs-semver-check/0.5.0 (+https://github.com/frames-sg/wsi-rs)"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/wsi-rs-semver.XXXXXX")"

cleanup() {
  if command -v trash >/dev/null 2>&1; then
    trash "$work_dir"
  elif command -v gio >/dev/null 2>&1; then
    gio trash "$work_dir"
  else
    printf 'warning: no trash command available; semver work directory retained: %s\n' \
      "$work_dir" >&2
  fi
}
trap cleanup EXIT

archive="$work_dir/wsi-rs-${BASELINE_VERSION}.crate"
curl --fail --silent --show-error --location \
  --user-agent "$USER_AGENT" \
  --retry 3 \
  --connect-timeout 15 \
  --max-time 120 \
  "https://crates.io/api/v1/crates/wsi-rs/${BASELINE_VERSION}/download" \
  --output "$archive"

if command -v sha256sum >/dev/null 2>&1; then
  actual_sha256="$(sha256sum "$archive" | awk '{print $1}')"
else
  actual_sha256="$(shasum -a 256 "$archive" | awk '{print $1}')"
fi
if [[ "$actual_sha256" != "$BASELINE_SHA256" ]]; then
  printf 'baseline archive checksum mismatch: expected %s, got %s\n' \
    "$BASELINE_SHA256" "$actual_sha256" >&2
  exit 1
fi

tar --extract --file "$archive" --directory "$work_dir"
baseline_root="$work_dir/wsi-rs-${BASELINE_VERSION}"

build_rustdoc() {
  local root="$1"
  local profile="$2"
  local output="$3"

  (
    cd "$root"
    if [[ "$root" == "$repo_root" && "$profile" == "default" ]]; then
      RUSTDOCFLAGS="-Z unstable-options --output-format json" \
        cargo +nightly rustdoc --package wsi-rs --lib --locked
    elif [[ "$root" == "$repo_root" ]]; then
      RUSTDOCFLAGS="-Z unstable-options --output-format json" \
        cargo +nightly rustdoc --package wsi-rs --lib --features "$profile" --locked
    elif [[ "$profile" == "default" ]]; then
      RUSTDOCFLAGS="-Z unstable-options --output-format json" \
        cargo +nightly rustdoc --lib --locked
    else
      RUSTDOCFLAGS="-Z unstable-options --output-format json" \
        cargo +nightly rustdoc --lib --features "$profile" --locked
    fi
    cp target/doc/wsi_rs.json "$output"
  )
}

cd "$repo_root"
for profile in default cuda metal; do
  baseline_rustdoc="$work_dir/baseline-${profile}.json"
  current_rustdoc="$work_dir/current-${profile}.json"
  build_rustdoc "$baseline_root" "$profile" "$baseline_rustdoc"
  build_rustdoc "$repo_root" "$profile" "$current_rustdoc"

  cargo semver-checks check-release \
    --current-rustdoc "$current_rustdoc" \
    --baseline-rustdoc "$baseline_rustdoc" \
    --release-type minor
done
