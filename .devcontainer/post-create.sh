#!/usr/bin/env bash

set -u

log() {
  printf '[post-create] %s\n' "$1"
}

warn() {
  printf '[post-create] warning: %s\n' "$1" >&2
}

run_as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
    return
  fi

  if command -v sudo >/dev/null 2>&1; then
    sudo "$@"
    return
  fi

  return 1
}

ensure_dir() {
  local dir="$1"
  mkdir -p "$dir"
}

ensure_root_owned_setup() {
  local dir
  for dir in "$@"; do
    ensure_dir "$dir"
  done

  if run_as_root chown -R "$(id -un):$(id -gn)" "$@"; then
    return
  fi

  warn "could not adjust ownership for: $*"
}

maybe_install_fuse_overlayfs() {
  if command -v fuse-overlayfs >/dev/null 2>&1; then
    log "fuse-overlayfs already installed"
    return
  fi

  if ! command -v apt-get >/dev/null 2>&1; then
    warn "apt-get is unavailable; skipping fuse-overlayfs install"
    return
  fi

  if ! run_as_root true >/dev/null 2>&1; then
    warn "root access is unavailable; skipping fuse-overlayfs install"
    return
  fi

  log "installing fuse-overlayfs"
  if ! run_as_root apt-get update -qq; then
    warn "apt-get update failed; skipping fuse-overlayfs install"
    return
  fi

  if ! run_as_root apt-get install -y -qq fuse-overlayfs; then
    warn "apt-get install fuse-overlayfs failed"
  fi
}

maybe_install_claude() {
  if [ "${INSTALL_CLAUDE:-0}" != "1" ]; then
    return
  fi

  if command -v claude >/dev/null 2>&1; then
    log "claude already installed"
    return
  fi

  if ! command -v curl >/dev/null 2>&1; then
    warn "curl is unavailable; skipping claude install"
    return
  fi

  log "installing claude"
  if ! curl -fsSL https://claude.ai/install.sh | bash; then
    warn "claude install failed"
  fi
}

maybe_install_codex() {
  if [ "${INSTALL_CODEX:-0}" != "1" ]; then
    return
  fi

  if command -v codex >/dev/null 2>&1; then
    log "codex already installed"
    return
  fi

  if ! command -v pnpm >/dev/null 2>&1; then
    warn "pnpm is unavailable; skipping codex install"
    return
  fi

  ensure_dir "${PNPM_HOME}/.tools"
  log "installing codex"
  if ! pnpm add -g @openai/codex; then
    warn "codex install failed"
  fi
}

maybe_update_submodules() {
  if ! command -v git >/dev/null 2>&1; then
    warn "git is unavailable; skipping submodule update"
    return
  fi

  log "updating submodules"
  if ! git submodule update --init --recursive; then
    warn "git submodule update failed"
  fi
}

maybe_install_cargo_insta() {
  if ! command -v cargo >/dev/null 2>&1; then
    warn "cargo is unavailable; skipping cargo-insta install"
    return
  fi

  if cargo insta --version >/dev/null 2>&1; then
    log "cargo-insta already installed"
    return
  fi

  log "installing cargo-insta"
  if ! cargo install cargo-insta; then
    warn "cargo install cargo-insta failed"
  fi
}

main() {
  local home_dir="${HOME:-/home/vscode}"
  local pnpm_store_dir

  export PNPM_HOME="${PNPM_HOME:-${home_dir}/.local/share/pnpm}"
  export CODEX_HOME="${CODEX_HOME:-${home_dir}/.codex}"
  export CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-${home_dir}/.claude}"
  export PATH="${PATH}:${PNPM_HOME}"
  pnpm_store_dir="${PNPM_STORE_DIR:-${PNPM_HOME}/store}"

  ensure_root_owned_setup "${home_dir}/.local" "$PNPM_HOME" "$CODEX_HOME" "$CLAUDE_CONFIG_DIR"
  ensure_dir "${pnpm_store_dir}"
  ensure_dir "${PNPM_HOME}/global"
  ensure_dir "${PNPM_HOME}/global/5"
  ensure_dir "${PNPM_HOME}/.tools"

  if command -v pnpm >/dev/null 2>&1; then
    pnpm config set global-bin-dir "$PNPM_HOME" || warn "pnpm global-bin-dir setup failed"
    pnpm config set store-dir "$pnpm_store_dir" || warn "pnpm store-dir setup failed"
  fi

  maybe_install_fuse_overlayfs
  maybe_install_claude
  maybe_install_codex
  maybe_update_submodules
  maybe_install_cargo_insta
}

main "$@"
