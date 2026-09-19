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
          toolchain = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain;
          rust = (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
            extensions = pkgs.lib.unique (
              (toolchain.components or [ ])
              ++ [
                "rust-src"
                "rust-analyzer"
              ]
            );
            targets = pkgs.lib.unique ((toolchain.targets or [ ]) ++ [ "wasm32-unknown-unknown" ]);
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
          # nixpkgs still ships 12.3.4; pin the official static release until it catches up.
          pnpmRelease =
            {
              x86_64-linux = {
                arch = "x64";
                hash = "sha256:b72cfc2140e2f3380e26555a474f8e091a4431374face630a50b00e4db992ddb";
              };
              aarch64-linux = {
                arch = "arm64";
                hash = "sha256:26eba6156b6b47d8c589e2ed76af9b3d3a001d9d87dd4769754a1d09f07505bb";
              };
            }
            .${system};
          pnpm = pkgs.stdenvNoCC.mkDerivation (finalAttrs: {
            pname = "pnpm";
            version = "12.4.1";
            src = pkgs.fetchurl {
              url = "https://github.com/pnpm/pnpm/releases/download/v${finalAttrs.version}/pnpm-linux-${pnpmRelease.arch}-musl.tar.gz";
              hash = pnpmRelease.hash;
            };
            sourceRoot = ".";
            dontConfigure = true;
            dontBuild = true;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            installPhase = ''
              runHook preInstall
              mkdir -p $out/libexec/pnpm $out/bin
              cp -r pnpm dist $out/libexec/pnpm/
              makeWrapper $out/libexec/pnpm/pnpm $out/bin/pnpm
              makeWrapper $out/libexec/pnpm/pnpm $out/bin/pnpx --add-flags dlx
              runHook postInstall
            '';
          });
          tools =
            assert pkgs.lib.assertMsg (
              pnpm.version == pnpmVersion
            ) "Update the pnpm version and release hashes in flake.nix to match package.json (${pnpmVersion}).";
            [
              (pkgs.lib.hiPrio cargoShim)
              rust
              mbx
              pkgs.nodejs_24
              pnpm
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
            pnpm
            pnpmVersion
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
            shellHook = ''
              export NPM_CONFIG_PREFIX="''${NPM_CONFIG_PREFIX:-$HOME/.local/share/npm}"
              export PATH="$NPM_CONFIG_PREFIX/bin:$PATH"
            '';
          };
        }
      );
      checks = forAllSystems (
        system:
        let
          e = environments.${system};
        in
        {
          pnpm-version = e.pkgs.runCommand "celox-pnpm-version-check" { } ''
            test "$(${e.pnpm}/bin/pnpm --version)" = "${e.pnpmVersion}"
            touch $out
          '';
          rust-toolchain = e.pkgs.runCommand "celox-rust-toolchain-check" { } ''
            ${e.rust}/bin/cargo-fmt --version
            ${e.rust}/bin/cargo-clippy --version
            ${e.rust}/bin/rustfmt --version
            ${e.rust}/bin/clippy-driver --version
            touch $out
          '';
        }
      );
      formatter = forAllSystems (system: environments.${system}.pkgs.nixfmt);
    };
}
