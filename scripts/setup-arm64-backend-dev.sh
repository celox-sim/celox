#!/usr/bin/env bash
# Prepare the current checkout for native or QEMU-based ARM64 development.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"

EXECUTION="auto"
QEMU_CPU_MODEL="cortex-a53"
INSTALL_DEPS=1
UPDATE_SUBMODULES=1
RUN_PROBE=1
DRY_RUN=0
OPEN_SHELL=0

usage() {
    cat <<'EOF'
Usage: scripts/setup-arm64-backend-dev.sh [options]

Prepare the current Git checkout for ARM64 backend work. On ARM64 Linux/macOS
the generated code runs natively. On x86-64 Linux the script installs an
AArch64 cross toolchain and runs target binaries through QEMU user-mode.

This script never creates, switches, updates, or deletes Git branches and
worktrees. Create the desired development branch before invoking it.

Options:
  --execution MODE       auto, native, or qemu (default: auto)
  --qemu-cpu CPU         QEMU CPU model (default: cortex-a53 / ARMv8-A)
  --no-install-deps      Do not install missing Debian/Ubuntu QEMU packages
  --no-submodules        Do not initialize submodules in the current checkout
  --no-probe             Skip compiling and executing the ARM64 smoke test
  --shell                Enter an interactive target-configured shell afterward
  --dry-run              Print mutating commands without running them
  -h, --help             Show this help

After setup:
  source target/arm64-dev/activate
  cargo test -p celox-backend-arm64
  cargo test -p celox

The facade feature is disabled by default. For real ARM64 Linux CI, use
ubuntu-24.04-arm.
EOF
}

log() {
    printf '[arm64-dev] %s\n' "$1"
}

die() {
    printf '[arm64-dev] error: %s\n' "$1" >&2
    exit 1
}

print_command() {
    local argument
    printf '[arm64-dev] +'
    for argument in "$@"; do
        printf ' %q' "$argument"
    done
    printf '\n'
}

run() {
    if (( DRY_RUN )); then
        print_command "$@"
    else
        "$@"
    fi
}

run_in_repo() {
    if (( DRY_RUN )); then
        printf '[arm64-dev] + (cd %q &&' "$REPO_ROOT"
        local argument
        for argument in "$@"; do
            printf ' %q' "$argument"
        done
        printf ')\n'
    else
        (
            cd "$REPO_ROOT"
            "$@"
        )
    fi
}

while (( $# > 0 )); do
    case "$1" in
        --execution)
            (( $# >= 2 )) || die "--execution requires a value"
            EXECUTION="$2"
            shift 2
            ;;
        --qemu-cpu)
            (( $# >= 2 )) || die "--qemu-cpu requires a value"
            QEMU_CPU_MODEL="$2"
            shift 2
            ;;
        --no-install-deps)
            INSTALL_DEPS=0
            shift
            ;;
        --no-submodules)
            UPDATE_SUBMODULES=0
            shift
            ;;
        --no-probe)
            RUN_PROBE=0
            shift
            ;;
        --shell)
            OPEN_SHELL=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

case "$EXECUTION" in
    auto|native|qemu) ;;
    *) die "--execution must be auto, native, or qemu" ;;
esac

[[ -n "$QEMU_CPU_MODEL" && "$QEMU_CPU_MODEL" != *[[:space:]]* ]] \
    || die "--qemu-cpu must be one non-empty CPU model name"

HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"
TARGET=""
QEMU_SYSROOT="/usr/aarch64-linux-gnu"

if [[ "$EXECUTION" == auto ]]; then
    case "$HOST_OS:$HOST_ARCH" in
        Linux:aarch64|Linux:arm64|Darwin:arm64)
            EXECUTION="native"
            ;;
        Linux:x86_64|Linux:amd64)
            EXECUTION="qemu"
            ;;
        *)
            die "cannot run ARM64 on $HOST_OS/$HOST_ARCH; use an ARM64 host or x86-64 Linux with QEMU"
            ;;
    esac
fi

case "$EXECUTION:$HOST_OS:$HOST_ARCH" in
    native:Linux:aarch64|native:Linux:arm64)
        TARGET="aarch64-unknown-linux-gnu"
        ;;
    native:Darwin:arm64)
        TARGET="aarch64-apple-darwin"
        ;;
    qemu:Linux:x86_64|qemu:Linux:amd64)
        TARGET="aarch64-unknown-linux-gnu"
        ;;
    native:*)
        die "native execution requires an ARM64 Linux or macOS host"
        ;;
    qemu:*)
        die "the supported QEMU setup requires an x86-64 Debian/Ubuntu Linux host"
        ;;
esac

ensure_qemu_dependencies() {
    local -a missing=()
    command -v qemu-aarch64 >/dev/null 2>&1 || missing+=(qemu-user)
    command -v aarch64-linux-gnu-gcc >/dev/null 2>&1 || missing+=(gcc-aarch64-linux-gnu)
    [[ -e "$QEMU_SYSROOT/lib/ld-linux-aarch64.so.1" ]] || missing+=(libc6-dev-arm64-cross)

    if (( ${#missing[@]} == 0 )); then
        log "QEMU and the AArch64 GNU sysroot are available"
        return
    fi
    if (( ! INSTALL_DEPS )); then
        die "missing QEMU dependencies: ${missing[*]}"
    fi
    command -v apt-get >/dev/null 2>&1 \
        || die "missing ${missing[*]}; automatic installation requires apt-get"

    local -a elevate=()
    if (( EUID != 0 )); then
        command -v sudo >/dev/null 2>&1 \
            || die "missing ${missing[*]}; rerun as root or install them manually"
        elevate=(sudo)
    fi
    log "installing QEMU dependencies: ${missing[*]}"
    run "${elevate[@]}" apt-get update
    run "${elevate[@]}" apt-get install -y --no-install-recommends "${missing[@]}"
}

if [[ "$EXECUTION" == qemu ]]; then
    ensure_qemu_dependencies
fi

if (( UPDATE_SUBMODULES )); then
    log "initializing submodules"
    run_in_repo git submodule update --init --recursive
fi

command -v rustup >/dev/null 2>&1 || die "rustup is required"
log "installing Rust target $TARGET"
run_in_repo rustup target add "$TARGET"

ACTIVATION_DIR="$REPO_ROOT/target/arm64-dev"
ACTIVATION="$ACTIVATION_DIR/activate"

write_activation() {
    if (( DRY_RUN )); then
        log "would write $ACTIVATION"
        return
    fi

    mkdir -p "$ACTIVATION_DIR"
    {
        printf '# Generated by scripts/setup-arm64-backend-dev.sh\n'
        printf 'export CELOX_ARM64_EXECUTION=%q\n' "$EXECUTION"
        printf 'export CELOX_ARM64_TARGET=%q\n' "$TARGET"
        printf 'export CARGO_BUILD_TARGET=%q\n' "$TARGET"
        if [[ "$EXECUTION" == qemu ]]; then
            printf 'export QEMU_CPU=%q\n' "$QEMU_CPU_MODEL"
            printf 'export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=%q\n' \
                'aarch64-linux-gnu-gcc'
            printf 'export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER=%q\n' \
                "qemu-aarch64 -L $QEMU_SYSROOT"
        fi
    } > "$ACTIVATION"
}

write_activation

run_probe() {
    local probe_source="$ACTIVATION_DIR/probe.rs"
    local probe_binary="$ACTIVATION_DIR/probe"

    if (( DRY_RUN )); then
        log "would compile and run an $TARGET smoke-test binary"
        return
    fi

    cat > "$probe_source" <<'EOF'
fn main() {
    assert_eq!(std::env::consts::ARCH, "aarch64");
    println!("ARM64 execution probe passed");
}
EOF

    if [[ "$EXECUTION" == qemu ]]; then
        run_in_repo rustc --target "$TARGET" \
            -C linker=aarch64-linux-gnu-gcc "$probe_source" -o "$probe_binary"
        run env QEMU_CPU="$QEMU_CPU_MODEL" \
            qemu-aarch64 -L "$QEMU_SYSROOT" "$probe_binary"
    else
        run_in_repo rustc --target "$TARGET" "$probe_source" -o "$probe_binary"
        run "$probe_binary"
    fi
}

if (( RUN_PROBE )); then
    run_probe
fi

log "current checkout is ready for ARM64 development"
printf '\n  source %q\n' "$ACTIVATION"
printf '  cargo test -p celox-backend-arm64\n'
printf '  cargo test -p celox\n\n'
if [[ "$TARGET" == aarch64-apple-darwin ]]; then
    log "note: this validates macOS ARM64; use ubuntu-24.04-arm for the Linux ABI"
elif [[ "$EXECUTION" == qemu ]]; then
    log "QEMU CPU model: $QEMU_CPU_MODEL"
    log "note: use ubuntu-24.04-arm CI for final execution on real ARM64 hardware"
fi

if (( OPEN_SHELL )); then
    if (( DRY_RUN )); then
        log "would open an interactive target-configured shell in $REPO_ROOT"
    else
        # shellcheck disable=SC1090
        source "$ACTIVATION"
        cd "$REPO_ROOT"
        exec "${SHELL:-/bin/bash}" -i
    fi
fi
