# Development environment

The Linux development environment is defined in `flake.nix` and `flake.lock`.
It supports x86-64 and AArch64. The host and both devcontainer variants use the
same tools: Rust, Node.js 24, pnpm, Python with cocotb, Verilator, C/C++ build
tools, cargo-insta, and mbx. Rust reads `rust-toolchain.toml`; evaluation fails
if the locked nixpkgs pnpm differs from `package.json`.

## Enter the environment

Install Nix with `nix-command` and `flakes` enabled, then run:

```bash
nix develop
pnpm install --frozen-lockfile
cargo test --locked
```

For scripts, use `nix develop --command <command>`, for example:

```bash
nix develop --command pnpm run build:napi
nix develop --command cargo test --locked -p celox-vpi --test cocotb_e2e \
  cocotb_drives_an_attached_native_flip_flop -- --exact --ignored
```

The shell sets `CELOX_COCOTB_PYTHON` to its Python with cocotb. It includes
`wasm32-unknown-unknown` for browser Rust builds. The separate NAPI WASI
cross-build still needs the WASI target and SDK used by CI; these are not
part of this native development shell.

For automatic activation, install direnv and nix-direnv once:

```bash
nix profile install nixpkgs#direnv nixpkgs#nix-direnv
mkdir -p ~/.config/direnv
# Add this line to ~/.config/direnv/direnvrc:
# source "$HOME/.nix-profile/share/nix-direnv/direnvrc"
# Add this line to ~/.bashrc (use `direnv hook zsh` for zsh):
# eval "$(direnv hook bash)"
direnv allow
```

The checked-in `.envrc` loads the flake through nix-direnv. Review and allow it
again when it changes. Launch editors from the activated shell, or use an
editor's direnv integration, so subprocesses get the same tools.

## mbx and filesystem placement

Inside the development environment, `cargo` is an mbx shim. This also applies
to Cargo invoked by pnpm or rust-analyzer. The real Cargo and Rust toolchain
remain on PATH; no global `mbx setup` or Cargo configuration is modified.
`mbx doctor` may still warn that global setup is missing: the shell uses a
scoped Cargo symlink. Check `readlink -f "$(command -v cargo)"` inside the
shell; it should resolve to the Nix-provided mbx binary.

mbx keeps reading your per-user configuration. On the existing development
host, `~/mbx` is a mounted Btrfs filesystem and `~/.config/mbx/config.toml`
already selects its `cache` directory. The flake does not override it.
Machine-specific absolute paths do not belong in `flake.nix` or `.mbx.toml`.

On another Linux host, create a writable directory on your chosen local
filesystem and add this to `~/.config/mbx/config.toml` (use your actual absolute
path; TOML does not expand `$HOME`):

```toml
cache_dir = "/home/YOUR_USER/mbx/cache"

# Optional limits; choose these for your disk and workload.
[gc]
max_total_size = "50GiB"

[target]
max_age = "30d"
```

Alternatively, export `MBX_CACHE_DIR=/your/mounted/filesystem/cache` before
entering the shell. This takes precedence over the user configuration.
Managed targets default to a directory inside the cache, so cache blobs and
restored outputs stay on the same filesystem. Avoid setting `CARGO_TARGET_DIR`,
Cargo's `build.target-dir`, or a separate `MBX_TARGET_ROOT`: explicit Cargo
targets bypass managed placement, and another filesystem prevents reflinks.

```bash
mbx doctor
cargo check --locked -p celox-state-layout
readlink -f target
mbx gc --dry-run
```

For a new checkout, the first build creates `target` as a symlink into the
managed cache. If a real `target/` already exists, mbx asks interactively before
replacing its outputs; non-interactive builds leave it alone. Stop active
builds before migrating. To discard old build outputs explicitly, use
`cargo clean` and remove an empty `target/` if necessary, then build again.
Do not delete the shared cache to migrate one checkout.

Btrfs is optional: mbx copies when reflinks are unavailable, so caching and
collection still work, but shared outputs can take more physical space.
Verify both cache and managed-target reflinks with `mbx doctor`. GC budgets
use logical bytes and are not hard limits on physical disk consumption.

### Provisioning a Btrfs cache

If an existing writable Btrfs mount is available, use a directory on it.
Otherwise, an administrator can provision a dedicated filesystem or a loopback
image once, mount it persistently through `/etc/fstab`, and give your user
ownership of a cache directory. Normal builds and reflinks need no sudo.
Neither the flake nor devcontainer mounts a block filesystem.

For the existing loopback setup, check the mount before building:

```bash
findmnt --target "$HOME/mbx"
df -h "$HOME/mbx"
```

The filesystem type should be `btrfs`, not the parent filesystem. A loopback
image consumes space on its backing filesystem too; monitor both. Nix's
`/nix/store` and its garbage collection are separate from mbx's storage budget.

## Devcontainer

Both `.devcontainer/default` and `.devcontainer/with-claude` directly use the
Ubuntu 24.04 devcontainer base image, install Nix, and build `.#dev-tools`
from the same lockfile. The resulting profile is on the
editor's PATH, and Bash loads the full devShell environment. Tool installation
failures stop setup. Optional assistant installations retain their existing
opt-in behavior and are outside the pinned development toolchain.

Before opening the container, ensure **`$HOME/mbx` exists on the Docker host**
and, if using Btrfs, that it is mounted. Both variants bind that directory to
`/home/vscode/mbx` and set
`MBX_CACHE_DIR=/home/vscode/mbx/container-cache`. The container cache and managed
targets are on the same host filesystem, without granting mount privileges to
the container. Docker Desktop or remote filesystem sharing may not preserve
reflink support; `mbx doctor` inside the container is the definitive check.
The host directory must be writable by the container user (normally the
devcontainer aligns its UID with the local user).

For a different host path, edit the mbx `mounts` entry in your chosen
`devcontainer.json`. You can use `source=${localEnv:CELOX_MBX_ROOT}` and set
`CELOX_MBX_ROOT` in the environment that launches VS Code / the devcontainer
CLI. Keep the destination and `MBX_CACHE_DIR` unchanged. With a remote Docker
daemon, the source must exist on that daemon's host.

Host and container paths differ. A `target` symlink created on the host may
point outside the container, and vice versa. Prefer separate checkouts for
host and container builds. Keep their cache roots separate as configured:
mbx collects targets whose checkout paths no longer exist, and each environment
sees different absolute paths. Worktrees within one environment still share
its cache. Both cache roots use the same Btrfs filesystem.
If reusing one checkout, stop builds and remove only the `target` symlink
(`unlink target`) before switching environments, then let mbx recreate it.
Never use recursive deletion on the symlink's resolved cache directory.

After creating the container, run `mbx doctor` and a build in its terminal.
Re-run `bash .devcontainer/post-create.sh` after changing the flake/lockfile,
then restart terminals and reload the editor to refresh its tool profile.

## Updating and validating the environment

```bash
nix flake update
nix flake check
nix fmt -- --check flake.nix nix/mbx.nix
nix develop --command bash -c 'rustc --version; node --version; pnpm --version; mbx doctor'
```

Commit `flake.lock` together with environment changes. When updating Rust,
update the rust-overlay input if its locked revision predates the release.
When updating pnpm, select a nixpkgs revision providing the version pinned in
`package.json`. mbx release URLs and per-architecture hashes live in
`nix/mbx.nix`; update both hashes when changing its version.

The shell pins development tools, not the complete host OS or all build inputs.
Use the committed Cargo/pnpm lockfiles and locked dependency installation for
repeatable builds. Existing CI workflows continue to test their platform
matrix independently of the Linux devShell.
