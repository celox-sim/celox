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
      url = "github:oxalica/rust-overlay/b0829a2cc9f7b596583331d78a942c1e5170d8a0";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      nix-packages,
      rust-overlay,
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
          default = (e.pkgs.mkShell.override { stdenv = e.stdenv; }) {
            packages = e.tools;
            CELOX_COCOTB_PYTHON = "${e.python}/bin/python3";
            Z3_BIN = "${e.pkgs.z3}/bin/z3";
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
