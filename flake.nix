{
  description = "Celox development tools and shared Cargo cache";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-packages = {
      url = "github:tignear/nix-packages";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      # Pin the revision explicitly so Renovate can group overlay and Rust updates.
      url = "github:oxalica/rust-overlay/c7e9d512653a35ef4d5879c3df4d44d7b78d0e6d";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The xezim reference simulator for the shared test suite, pinned by
    # revision so that Renovate proposes updates.
    xezim = {
      url = "github:aionhw/xezim/5eab5f276c9062cfd17db4987a1e91de74fdbe5d";
      flake = false;
    };
  };

  outputs =
    {
      nixpkgs,
      nix-packages,
      rust-overlay,
      xezim,
      ...
    }:
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
            # Only the proprietary Mergify CLI may bypass the free-license check.
            config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "mergify-cli";
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
            targets = pkgs.lib.unique (
              (toolchain.targets or [ ])
              ++ [
                "wasm32-unknown-unknown"
                "wasm32-wasip1-threads"
              ]
            );
          };
          mbx = nix-packages.packages.${system}.mbx;
          mergify-cli = pkgs.callPackage ./nix/mergify-cli.nix { };
          xezim-simulator = pkgs.callPackage ./nix/xezim { src = xezim; };
          # mbx recognizes a symlink named cargo and preserves Cargo's CLI,
          # including --version. Real Cargo must remain later on PATH.
          cargoShim = pkgs.runCommand "celox-cargo-shim" { } ''
            mkdir -p $out/bin
            ln -s ${mbx}/bin/mbx $out/bin/cargo
          '';
          python = pkgs.python313.withPackages (ps: [
            ps.cocotb
            ps.psutil
          ]);
          stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;
          # Corepack resolves pnpm from package.json at runtime, independently of nixpkgs.
          tools = [
            (pkgs.lib.hiPrio cargoShim)
            rust
            mbx
            pkgs.nodejs_24
            (pkgs.lib.hiPrio pkgs.corepack)
            python
            pkgs.verilator
            pkgs.iverilog
            pkgs.z3
            stdenv.cc
            pkgs.mold
            pkgs.gnumake
            pkgs.cmake
            pkgs.pkg-config
            pkgs.openssl
            pkgs.cargo-deny
            pkgs.cargo-insta
            pkgs.cargo-nextest
            pkgs.git
            pkgs.curl
            pkgs.jq
            mergify-cli
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
            mergify-cli
            xezim-simulator
            tools
            python
            rust
            stdenv
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
          inherit (e) mbx mergify-cli;
          xezim = e.xezim-simulator;
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
          shell =
            extra:
            (e.pkgs.mkShell.override { stdenv = e.stdenv; }) {
              packages = e.tools ++ extra;
              CELOX_COCOTB_PYTHON = "${e.python}/bin/python3";
              Z3_BIN = "${e.pkgs.z3}/bin/z3";
              shellHook = ''
                export NPM_CONFIG_PREFIX="''${NPM_CONFIG_PREFIX:-$HOME/.local/share/npm}"
                export PATH="$NPM_CONFIG_PREFIX/bin:$PATH"
              '';
            };
        in
        {
          default = shell [ ];
          # xezim builds from source, so only its external-suite jobs use it.
          xezim = shell [ e.xezim-simulator ];
        }
      );
      checks = forAllSystems (
        system:
        let
          e = environments.${system};
        in
        {
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
