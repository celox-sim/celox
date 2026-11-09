# Verilator pinned to the latest stable release. nixpkgs (both the locked
# revision and nixpkgs-unstable) currently ships 5.052, so override the
# source until nixpkgs catches up; drop this override at that point.
{ verilator, fetchurl }:
let
  version = "5.054";
in
verilator.overrideAttrs (old: {
  inherit version;
  src = fetchurl {
    url = "https://github.com/verilator/verilator/archive/refs/tags/v${version}.tar.gz";
    hash = "sha256-GGWHMbRpIu9mC8tlC+CqUD11HlwX651GRP36uxGDUZg=";
  };
})
