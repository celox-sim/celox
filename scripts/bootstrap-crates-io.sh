#!/usr/bin/env bash
set -euo pipefail

mode="${1:-package}"
confirmation="${2:-}"
if [[ "$mode" != "package" && "$mode" != "publish" ]]; then
  echo "usage: $0 [package|publish] [BOOTSTRAP-CELOX-CRATES]" >&2
  exit 2
fi

if [[ "$mode" == "publish" ]]; then
  if [[ "$confirmation" != "BOOTSTRAP-CELOX-CRATES" ]]; then
    echo "publishing placeholder crates is permanent" >&2
    echo "pass BOOTSTRAP-CELOX-CRATES as the second argument to continue" >&2
    exit 2
  fi
  : "${CARGO_REGISTRY_TOKEN:?CARGO_REGISTRY_TOKEN is required for publication}"
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
mapfile -t crates < <("$script_dir/publish-crates.sh" list)
if [[ ${#crates[@]} -eq 0 ]]; then
  echo "no public crates were found" >&2
  exit 1
fi

bootstrap_dir="$(mktemp -d -t celox-crates-bootstrap.XXXXXXXXXX)"
cleanup() {
  if [[ -n "${bootstrap_dir:-}" && -d "$bootstrap_dir" && ! -L "$bootstrap_dir" ]]; then
    local name
    name="$(basename -- "$bootstrap_dir")"
    if [[ "$name" == celox-crates-bootstrap.* ]]; then
      rm -rf -- "$bootstrap_dir"
    fi
  fi
}
trap cleanup EXIT

crate_exists() {
  local crate="$1"
  curl --fail --silent --show-error \
    --user-agent "celox-crates-bootstrap/0.0.0 (https://github.com/celox-sim/celox)" \
    "https://crates.io/api/v1/crates/$crate" \
    >/dev/null 2>&1
}

placeholder_exists() {
  local crate="$1"
  curl --fail --silent --show-error \
    --user-agent "celox-crates-bootstrap/0.0.0 (https://github.com/celox-sim/celox)" \
    "https://crates.io/api/v1/crates/$crate/0.0.0" \
    >/dev/null 2>&1
}

wait_for_placeholder() {
  local crate="$1"
  for _ in {1..12}; do
    if placeholder_exists "$crate"; then
      return 0
    fi
    sleep 5
  done
  echo "$crate@0.0.0 was published but did not become visible on crates.io" >&2
  return 1
}

for crate in "${crates[@]}"; do
  if [[ ! "$crate" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
    echo "invalid crate name in publication list: $crate" >&2
    exit 1
  fi

  crate_dir="$bootstrap_dir/$crate"
  mkdir -p "$crate_dir/src"
  printf '%s\n' \
    '[package]' \
    "name = \"$crate\"" \
    'version = "0.0.0"' \
    'edition = "2024"' \
    'license = "MIT OR Apache-2.0"' \
    'description = "Trusted Publishing bootstrap placeholder for the Celox simulator"' \
    'repository = "https://github.com/celox-sim/celox"' \
    'readme = "README.md"' \
    'publish = ["crates-io"]' \
    >"$crate_dir/Cargo.toml"
  # The backticks are intentional Markdown and Rustdoc literals.
  # shellcheck disable=SC2016
  printf '# %s\n\nThis `0.0.0` package reserves the crate for the [Celox](https://github.com/celox-sim/celox) release workflow while crates.io Trusted Publishing is configured. Use a stable release for actual code.\n' \
    "$crate" >"$crate_dir/README.md"
  # shellcheck disable=SC2016
  printf '#![doc = "Trusted Publishing bootstrap placeholder for `%s`."]\n' \
    "$crate" >"$crate_dir/src/lib.rs"

  if [[ "$mode" == "package" ]]; then
    echo "validating $crate@0.0.0 placeholder"
    cargo package --manifest-path "$crate_dir/Cargo.toml" >/dev/null
    continue
  fi

  if placeholder_exists "$crate"; then
    echo "$crate@0.0.0 already exists; skipping"
    continue
  fi
  if crate_exists "$crate"; then
    echo "$crate already exists without the expected 0.0.0 placeholder; stopping" >&2
    exit 1
  fi

  echo "publishing $crate@0.0.0 placeholder"
  cargo publish --manifest-path "$crate_dir/Cargo.toml"
  wait_for_placeholder "$crate"
done
