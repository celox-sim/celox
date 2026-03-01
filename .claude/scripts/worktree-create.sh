#!/usr/bin/env bash
set -euo pipefail

# WorktreeCreate hook for Claude Code
# Creates a git worktree and mounts overlayfs over target/ and node_modules/
# so that heavy build caches and dependencies are shared copy-on-write.
#
# Input (JSON on stdin): { "name": "...", "cwd": "...", ... }
# Output (stdout):       absolute path to the created worktree

INPUT=$(cat)
NAME=$(echo "$INPUT" | jq -r '.name')
MAIN_DIR=$(echo "$INPUT" | jq -r '.cwd')

WORKTREE_DIR="$MAIN_DIR/.claude/worktrees/$NAME"
OVERLAY_BASE="$WORKTREE_DIR/.overlays"

# 1. Create the git worktree
git -C "$MAIN_DIR" worktree add -b "claude/$NAME" "$WORKTREE_DIR" HEAD --quiet

# 2. Mount overlayfs layers if fuse-overlayfs is available
if command -v fuse-overlayfs >/dev/null 2>&1; then
  # Mount an overlayfs for a directory (relative path from repo root)
  mount_overlay() {
    local rel_path="$1"
    local src="$MAIN_DIR/$rel_path"
    local dst="$WORKTREE_DIR/$rel_path"

    [ -d "$src" ] || return 0

    local key
    key=$(echo "$rel_path" | sed 's|/|__|g')
    local upper="$OVERLAY_BASE/$key/upper"
    local work="$OVERLAY_BASE/$key/work"

    mkdir -p "$dst" "$upper" "$work"
    fuse-overlayfs \
      -o "lowerdir=$src,upperdir=$upper,workdir=$work" \
      "$dst"
  }

  # Rust build cache
  mount_overlay "target"

  # node_modules directories (pnpm workspace)
  while IFS= read -r nm_dir; do
    mount_overlay "${nm_dir#"$MAIN_DIR"/}"
  done < <(find "$MAIN_DIR" \
    -path "$MAIN_DIR/.claude" -prune -o \
    -name node_modules -type d -print -prune)
fi

# 3. Print the worktree path (required by Claude Code)
echo "$WORKTREE_DIR"
