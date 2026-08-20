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
  if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required to configure crates.io Trusted Publishing" >&2
    exit 1
  fi
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

crates_io_api="https://crates.io/api/v1"
trusted_repository_owner="celox-sim"
trusted_repository_name="celox"
trusted_workflow_filename="publish-crates.yml"
trusted_environment="crates-io"
curl_auth_config="$bootstrap_dir/curl-auth.conf"

if [[ "$mode" == "publish" ]]; then
  if [[ "$CARGO_REGISTRY_TOKEN" == *$'\n'* || "$CARGO_REGISTRY_TOKEN" == *$'\r'* ]]; then
    echo "CARGO_REGISTRY_TOKEN must not contain a newline" >&2
    exit 1
  fi
  (
    umask 077
    printf 'header = "Authorization: %s"\n' "$CARGO_REGISTRY_TOKEN" >"$curl_auth_config"
  )
fi

crate_exists() {
  local crate="$1"
  curl --fail --silent --show-error \
    --user-agent "celox-crates-bootstrap/0.0.0 (https://github.com/celox-sim/celox)" \
    "$crates_io_api/crates/$crate" \
    >/dev/null 2>&1
}

placeholder_exists() {
  local crate="$1"
  curl --fail --silent --show-error \
    --user-agent "celox-crates-bootstrap/0.0.0 (https://github.com/celox-sim/celox)" \
    "$crates_io_api/crates/$crate/0.0.0" \
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

trusted_publisher_configs() {
  local crate="$1"
  curl --config "$curl_auth_config" \
    --fail-with-body --silent --show-error --get \
    --data-urlencode "crate=$crate" \
    --user-agent "celox-crates-bootstrap/0.0.0 (https://github.com/celox-sim/celox)" \
    "$crates_io_api/trusted_publishing/github_configs"
}

ensure_trusted_publisher() {
  local crate="$1"
  local configs
  configs="$(trusted_publisher_configs "$crate")"

  if ! jq -e '.github_configs | type == "array"' <<<"$configs" >/dev/null; then
    echo "crates.io returned an invalid Trusted Publishing response for $crate" >&2
    return 1
  fi

  local config_count
  local matching_count
  config_count="$(jq '.github_configs | length' <<<"$configs")"
  matching_count="$(
    jq \
      --arg owner "$trusted_repository_owner" \
      --arg repository "$trusted_repository_name" \
      --arg workflow "$trusted_workflow_filename" \
      --arg environment "$trusted_environment" \
      '[.github_configs[] | select(
        .repository_owner == $owner and
        .repository_name == $repository and
        .workflow_filename == $workflow and
        .environment == $environment
      )] | length' <<<"$configs"
  )"

  if [[ "$config_count" -eq 1 && "$matching_count" -eq 1 ]]; then
    echo "$crate Trusted Publisher is already configured; skipping"
    return 0
  fi

  if [[ "$config_count" -ne 0 ]]; then
    echo "$crate has unexpected Trusted Publisher configuration; stopping" >&2
    jq -r \
      '.github_configs[] | "  \(.repository_owner)/\(.repository_name) \(.workflow_filename) environment=\(.environment // "(none)")"' \
      <<<"$configs" >&2
    return 1
  fi

  local payload
  payload="$(
    jq -cn \
      --arg crate "$crate" \
      --arg owner "$trusted_repository_owner" \
      --arg repository "$trusted_repository_name" \
      --arg workflow "$trusted_workflow_filename" \
      --arg environment "$trusted_environment" \
      '{github_config: {
        crate: $crate,
        repository_owner: $owner,
        repository_name: $repository,
        workflow_filename: $workflow,
        environment: $environment
      }}'
  )"

  local response
  response="$(
    curl --config "$curl_auth_config" \
      --fail-with-body --silent --show-error \
      --request POST \
      --header 'Content-Type: application/json' \
      --data "$payload" \
      --user-agent "celox-crates-bootstrap/0.0.0 (https://github.com/celox-sim/celox)" \
      "$crates_io_api/trusted_publishing/github_configs"
  )"

  if ! jq -e \
    --arg crate "$crate" \
    --arg owner "$trusted_repository_owner" \
    --arg repository "$trusted_repository_name" \
    --arg workflow "$trusted_workflow_filename" \
    --arg environment "$trusted_environment" \
    '.github_config |
      .crate == $crate and
      .repository_owner == $owner and
      .repository_name == $repository and
      .workflow_filename == $workflow and
      .environment == $environment' <<<"$response" >/dev/null; then
    echo "crates.io returned an invalid Trusted Publishing configuration for $crate" >&2
    return 1
  fi

  echo "configured $crate Trusted Publisher"
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
    echo "$crate@0.0.0 already exists; skipping publication"
    ensure_trusted_publisher "$crate"
    continue
  fi
  if crate_exists "$crate"; then
    echo "$crate already exists without the expected 0.0.0 placeholder; stopping" >&2
    exit 1
  fi

  echo "publishing $crate@0.0.0 placeholder"
  cargo publish --manifest-path "$crate_dir/Cargo.toml"
  wait_for_placeholder "$crate"
  ensure_trusted_publisher "$crate"
done
