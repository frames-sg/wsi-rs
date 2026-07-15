#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
publish_script="$repo_root/scripts/publish-crate.sh"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/wsi-rs-publish-test.XXXXXX")"

cleanup() {
  if command -v trash >/dev/null 2>&1; then
    trash "$temp_root"
  elif command -v gio >/dev/null 2>&1; then
    gio trash "$temp_root"
  else
    printf 'warning: no trash command available; temporary test directory retained: %s\n' \
      "$temp_root" >&2
  fi
}
trap cleanup EXIT

fail() {
  printf 'publish script test failed: %s\n' "$*" >&2
  exit 1
}

assert_fails() {
  local expected="$1"
  shift
  local output status
  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e
  [[ "$status" -ne 0 ]] || fail "command unexpectedly succeeded: $*"
  [[ "$output" == *"$expected"* ]] || \
    fail "failure did not contain '$expected': $output"
}

mkdir -p "$temp_root/bin"
cat >"$temp_root/Cargo.toml" <<'TOML'
[package]
name = "wsi-rs"
version = "0.5.0"
TOML
cat >"$temp_root/CHANGELOG.md" <<'MD'
# Changelog

## [0.5.0] - 2026-07-11
MD
cat >"$temp_root/bin/cargo" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FAKE_CARGO_LOG"
exit 0
SH
chmod +x "$temp_root/bin/cargo"

git -C "$temp_root" init -q
git -C "$temp_root" config user.name test
git -C "$temp_root" config user.email test@example.invalid
git -C "$temp_root" add Cargo.toml CHANGELOG.md
git -C "$temp_root" commit -qm initial

export PATH="$temp_root/bin:$PATH"
export FAKE_CARGO_LOG="$temp_root/cargo.log"
cd "$temp_root"

env -u GITHUB_REF_TYPE -u GITHUB_REF_NAME -u GITHUB_SHA \
  "$publish_script" --verify >/dev/null
env -u GITHUB_REF_TYPE -u GITHUB_REF_NAME -u GITHUB_SHA \
  "$publish_script" --dry-run >/dev/null
grep -Fxq 'publish --dry-run --locked' "$FAKE_CARGO_LOG" || \
  fail "dry run did not use --locked"

assert_fails "requires GITHUB_REF_TYPE=tag" \
  env GITHUB_REF_TYPE=branch GITHUB_REF_NAME=main "$publish_script" --publish

git tag v0.4.0
assert_fails "does not match manifest version v0.5.0" \
  env GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v0.4.0 "$publish_script" --publish

git tag v0.5.0
assert_fails "OIDC-provided CARGO_REGISTRY_TOKEN is required" \
  env -u CARGO_REGISTRY_TOKEN GITHUB_REF_TYPE=tag GITHUB_REF_NAME=v0.5.0 \
  GITHUB_SHA="$(git rev-parse HEAD)" "$publish_script" --publish

printf 'publish script behavior tests passed\n'
