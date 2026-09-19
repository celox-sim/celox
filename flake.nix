{
  description = "Celox development tools and shared Cargo cache";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      environments = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          rust = (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
            targets = [ "wasm32-unknown-unknown" ];
          };
          mbx = import ./nix/mbx.nix { inherit pkgs; };
          # mbx recognizes a symlink named cargo and preserves Cargo's CLI,
          # including --version. Real Cargo must remain later on PATH.
          cargoShim = pkgs.runCommand "celox-cargo-shim" { } ''
            mkdir -p $out/bin
            ln -s ${mbx}/bin/mbx $out/bin/cargo
          '';
          python = pkgs.python313.withPackages (ps: [ ps.cocotb ]);
          pnpmVersion = nixpkgs.lib.removePrefix "pnpm@" (builtins.fromJSON (
            builtins.readFile ./package.json
          )).packageManager;
          tools =
            assert pkgs.pnpm.version == pnpmVersion;
            [
              (pkgs.lib.hiPrio cargoShim)
              rust
              mbx
              pkgs.nodejs_24
              pkgs.pnpm
              python
              pkgs.verilator
              pkgs.stdenv.cc
              pkgs.gnumake
              pkgs.cmake
              pkgs.pkg-config
              pkgs.openssl
              pkgs.fuse-overlayfs
              pkgs.cargo-insta
              pkgs.git
              pkgs.curl
              pkgs.jq
              pkgs.direnv
              pkgs.nix-direnv
              pkgs.nixfmt
              pkgs.shellcheck
            ];
        in
        {
          inherit
            pkgs
            mbx
            tools
            python
            rust
            ;
        }
      );
    in
    {
      packages = forAllSystems (
        system:
        let
          e = environments.${system};
        in
        {
          inherit (e) mbx;
          # The devcontainer uses this profile for editor and non-shell commands.
          dev-tools = e.pkgs.buildEnv {
            name = "celox-dev-tools";
            paths = e.tools;
            postBuild = ''
              mkdir -p $out/libexec
              ln -s ${e.rust} $out/libexec/rust
            '';
            pathsToLink = [
              "/bin"
              "/share/nix-direnv"
            ];
          };
        }
      );
      devShells = forAllSystems (
        system:
        let
          e = environments.${system};
        in
        {
          default = e.pkgs.mkShell {
            packages = e.tools;
            CELOX_COCOTB_PYTHON = "${e.python}/bin/python3";
          };
        }
      );
      formatter = forAllSystems (system: environments.${system}.pkgs.nixfmt);
    };
}
