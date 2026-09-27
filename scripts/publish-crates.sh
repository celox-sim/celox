#!/usr/bin/env bash
set -euo pipefail

mode="${1:-package}"
if [[ "$mode" != "preflight" && "$mode" != "list" && "$mode" != "package" && "$mode" != "publish" ]]; then
  echo "usage: $0 [list|preflight|package|publish]" >&2
  exit 2
fi

version="$(tr -d '\r\n' < VERSION)"
if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "VERSION must contain a stable SemVer version, got $version" >&2
  exit 1
fi

# Dependency order matters: crates.io only accepts dependencies that have
# already been published. Keep each adapter after the crates it links.
crates=(
  celox-test-suite-veryl
  celox-analysis
  celox-backend-common
  celox-design
  celox-frontend-sdk
  celox-macros
  celox-ts-gen
  celox-sv-analyzer
  celox-testbench
  celox-sir
  celox-state-layout
  celox-slt
  celox-backend-cranelift
  celox-backend-wasm
  celox-frontend-core
  celox-runtime
  celox-sir-opt
  celox-backend-x86
  celox-backend-arm64
  celox-frontend-sv
  celox-frontend-veryl
  celox
  celox-napi
)

if [[ "$mode" == "list" ]]; then
  printf '%s\n' "${crates[@]}"
  exit 0
fi

if [[ "$mode" == "publish" ]]; then
  : "${CARGO_REGISTRY_TOKEN:?CARGO_REGISTRY_TOKEN is required for publication}"
fi

# Only a 404 means absent: authentication, rate limits and network failures
# must not be mistaken for an unpublished crate.
registry_exists() {
  local path="$1"
  local status
  status="$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' \
    --user-agent "celox-release-workflow/$version (https://github.com/celox-sim/celox)" \
    "https://crates.io/api/v1/crates/$path")" || return 2
  case "$status" in
    200) return 0 ;;
    404) return 1 ;;
    *) echo "crates.io lookup for $path failed (HTTP $status)" >&2; return 2 ;;
  esac
}

preflight() {
  local crate status missing=false
  for crate in "${crates[@]}"; do
    if registry_exists "$crate"; then
      continue
    else
      status=$?
    fi
    if [[ $status -ne 1 ]]; then
      return "$status"
    fi
    echo "$crate has not been bootstrapped on crates.io" >&2
    missing=true
  done
  if [[ "$missing" == true ]]; then
    echo "Trusted Publishing cannot create new crates. Complete the local bootstrap in .github/RELEASING.md (First crates.io release), then retry." >&2
    return 1
  fi
}

if [[ "$mode" == "preflight" || "$mode" == "publish" ]]; then
  preflight
  if [[ "$mode" == "preflight" ]]; then
    exit 0
  fi
fi

crate_exists() {
  local crate="$1"
  registry_exists "$crate/$version"
}

wait_for_crate() {
  local crate="$1" status
  for _ in {1..12}; do
    if crate_exists "$crate"; then
      return 0
    else
      status=$?
      if [[ $status -ne 1 ]]; then
        return "$status"
      fi
    fi
    sleep 5
  done
  echo "$crate@$version was published but did not become visible on crates.io" >&2
  return 1
}

for crate in "${crates[@]}"; do
  if [[ "$mode" == "package" ]]; then
    # `cargo package` resolves normalized dependencies from crates.io, so a full
    # archive cannot be created for dependent crates before their first release.
    # Check the file list here; the ordered publish pass builds and tests each
    # real archive once its same-version dependencies are available.
    echo "checking package file list for $crate@$version"
    cargo package --locked --allow-dirty --no-verify --list -p "$crate" >/dev/null
    continue
  fi

  if crate_exists "$crate"; then
    echo "$crate@$version is already published; skipping"
    continue
  else
    status=$?
    if [[ $status -ne 1 ]]; then
      exit "$status"
    fi
  fi

  echo "building and checking package archive for $crate@$version"
  cargo package --locked -p "$crate"
  package_dir="$PWD/target/package/$crate-$version"
  if [[ ! -f "$package_dir/Cargo.toml" || -L "$package_dir" ]]; then
    echo "cargo did not create the expected package directory: $package_dir" >&2
    exit 1
  fi
  # The normal release checks already run the workspace tests. Check every
  # target from the normalized archive here without linking a second copy of
  # every test and benchmark binary; linking all of Celox exceeds the disk on a
  # GitHub-hosted runner. A shared target directory also reuses dependencies
  # between archives instead of keeping one full build tree per crate.
  cargo check \
    --locked \
    --all-targets \
    --manifest-path "$package_dir/Cargo.toml" \
    --target-dir "$PWD/target/package-checks"

  published=false
  for attempt in {1..6}; do
    set +e
    output="$(cargo publish --locked -p "$crate" 2>&1)"
    status=$?
    set -e
    printf '%s\n' "$output"

    if [[ $status -eq 0 ]]; then
      published=true
      break
    fi
    if grep -Eqi 'already (exists|uploaded)|already been uploaded' <<<"$output"; then
      published=true
      break
    fi
    if grep -Fq 'Trusted Publishing tokens do not support creating new crates' <<<"$output"; then
      echo "Complete the local bootstrap in .github/RELEASING.md before retrying." >&2
      exit 1
    fi
    if [[ $attempt -lt 6 ]]; then
      echo "publish attempt $attempt for $crate failed; retrying in 15 seconds" >&2
      sleep 15
    fi
  done

  if [[ "$published" != true ]]; then
    echo "failed to publish $crate@$version" >&2
    exit 1
  fi
  wait_for_crate "$crate"
done
