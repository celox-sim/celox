#!/usr/bin/env bash

set -eu

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

  log "building development tools from flake.lock"
  ensure_dir "${home_dir}/.local/share/celox"
  nix build --no-update-lock-file .#dev-tools \
    --out-link "${home_dir}/.local/share/celox/dev-tools"
  export PATH="${home_dir}/.local/share/celox/dev-tools/bin:${home_dir}/.local/share/celox/dev-tools/libexec/rust/bin:${PATH}"
  nix print-dev-env --no-update-lock-file > "${home_dir}/.local/share/celox/env.sh"
  # print-dev-env defines shellHook but does not execute it when sourced.
  # shellcheck disable=SC2016
  printf '\neval "$shellHook"\n' >> "${home_dir}/.local/share/celox/env.sh"
  if ! grep -qF '.local/share/celox/env.sh' "${home_dir}/.bashrc"; then
    # Expand HOME when the new terminal starts, not during setup.
    # shellcheck disable=SC2016
    printf '\nsource "$HOME/.local/share/celox/env.sh"\n' >> "${home_dir}/.bashrc"
  fi

  if command -v pnpm >/dev/null 2>&1; then
    pnpm config set global-bin-dir "$PNPM_HOME" || warn "pnpm global-bin-dir setup failed"
    pnpm config set store-dir "$pnpm_store_dir" || warn "pnpm store-dir setup failed"
  fi

  maybe_install_claude
  maybe_install_codex
  maybe_update_submodules
}

main "$@"
