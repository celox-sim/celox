#!/usr/bin/env bash
set -euo pipefail
version=${1:?version required}
revision=${2:?source commit required}
prefix=${3:?installation prefix required}
jobs=${4:-2}
if [[ ! "$version" =~ ^[0-9]+\.[0-9]{3}$ || ! "$revision" =~ ^[0-9a-f]{40}$ || ! "$jobs" =~ ^[1-9][0-9]*$ || "$prefix" != /* || "$prefix" == / ]]; then
  echo 'Invalid Verilator version, source commit, installation prefix or job count' >&2
  exit 1
fi
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
if [[ -e "$prefix/.celox-verilator-source" ]]; then
  bash "$script_dir/verify.sh" "$version" "$revision" "$prefix"
  exit 0
fi
source_dir=$(mktemp -d "${RUNNER_TEMP:-/tmp}/celox-verilator.XXXXXX")
git -C "$source_dir" init --quiet
git -C "$source_dir" fetch --quiet --depth=1 https://github.com/verilator/verilator.git "$revision"
git -C "$source_dir" checkout --quiet --detach FETCH_HEAD
[[ $(git -C "$source_dir" rev-parse HEAD) == "$revision" ]]
cd "$source_dir"
unset VERILATOR_ROOT VERILATOR_NO_OPT_BUILD CFLAGS CXXFLAGS CPPFLAGS LDFLAGS
export CC=gcc CXX=g++ OBJCACHE=''
autoconf
./configure --prefix="$prefix"
make -j "$jobs"
make install
printf '%s\n' "$revision" > "$prefix/.celox-verilator-source"
bash "$script_dir/verify.sh" "$version" "$revision" "$prefix"
