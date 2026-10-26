#!/usr/bin/env bash
# Regenerate Cargo.lock for the xezim revision pinned in flake.lock. Run from
# the repository root in the development shell after updating that input.
set -euo pipefail
here=$(dirname "$0")
src=$(nix eval --raw --no-update-lock-file .#xezim.src)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp -r "$src"/. "$work"
chmod -R u+w "$work"
# Resolve with nixpkgs' cargo and rustc, which build the package, rather than
# the development shell's toolchain.
mapfile -t toolchain < <(nix build --no-link --print-out-paths --no-update-lock-file .#xezim.cargo .#xezim.rustc)
PATH="$(printf '%s/bin:' "${toolchain[@]}")$PATH"
cargo --version
rustc --version
cargo generate-lockfile --manifest-path "$work/Cargo.toml"
cp "$work/Cargo.lock" "$here/Cargo.lock"
