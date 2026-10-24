# xezim, an IEEE 1800 simulator, as an external reference for the shared test
# suite. `src` is the `xezim` flake input, pinned by revision and updated by
# Renovate. Upstream does not commit a Cargo.lock; the one here resolves that
# revision, and `update-lock.sh` regenerates it after an update. Its git
# dependency xezim-core is fetched at the revision the lockfile records.
{
  lib,
  rustPlatform,
  src,
}:
rustPlatform.buildRustPackage {
  pname = "xezim";
  version = (lib.importTOML "${src}/Cargo.toml").package.version;
  inherit src;

  cargoLock = {
    lockFile = ./Cargo.lock;
    allowBuiltinFetchGit = true;
  };
  postPatch = ''
    cp ${./Cargo.lock} Cargo.lock
  '';
  cargoBuildFlags = [
    "--bin"
    "xezim"
  ];
  # The suite's adapter tests exercise the binary; upstream's own test suite
  # needs network access and other simulators.
  doCheck = false;
  # update-lock.sh resolves the lockfile with the toolchain that builds it,
  # so the rust-version-aware resolver picks versions this rustc accepts.
  passthru = {
    inherit (rustPlatform.rust) cargo rustc;
  };

  meta = {
    description = "SystemVerilog simulator (IEEE 1800-2023)";
    homepage = "https://github.com/aionhw/xezim";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "xezim";
  };
}
