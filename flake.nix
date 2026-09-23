{
  description = "Celox development tools and shared Cargo cache";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-packages = {
      url = "github:tignear/nix-packages";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
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
          mbx = nix-packages.packages.${system}.mbx;
          # mbx recognizes a symlink named cargo and preserves Cargo's CLI,
          # including --version. Real Cargo must remain later on PATH.
          cargoShim = pkgs.runCommand "celox-cargo-shim" { } ''
            mkdir -p $out/bin
            ln -s ${mbx}/bin/mbx $out/bin/cargo
          '';
          python = pkgs.python313.withPackages (ps: [ ps.cocotb ]);
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
            stdenv.cc
            pkgs.mold
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
          default = (e.pkgs.mkShell.override { stdenv = e.stdenv; }) {
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
