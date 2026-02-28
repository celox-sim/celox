#!/usr/bin/env bash
set -euo pipefail

# WorktreeCreate hook for Claude Code
# Creates a git worktree and mounts an overlayfs over target/ so that
# the heavy Rust build cache is shared copy-on-write from the main repo.
#
# Input (JSON on stdin): { "name": "...", "cwd": "...", ... }
# Output (stdout):       absolute path to the created worktree

INPUT=$(cat)
NAME=$(echo "$INPUT" | jq -r '.name')
MAIN_DIR=$(echo "$INPUT" | jq -r '.cwd')

WORKTREE_DIR="$MAIN_DIR/.claude/worktrees/$NAME"
MAIN_TARGET="$MAIN_DIR/target"
WT_TARGET="$WORKTREE_DIR/target"
OVERLAY_UPPER="$WORKTREE_DIR/.overlay-upper"
OVERLAY_WORK="$WORKTREE_DIR/.overlay-work"

# 1. Create the git worktree
git -C "$MAIN_DIR" worktree add -b "claude/$NAME" "$WORKTREE_DIR" HEAD --quiet

# 2. Mount overlayfs over target/ if the main target exists
if [ -d "$MAIN_TARGET" ] && command -v fuse-overlayfs >/dev/null 2>&1; then
  mkdir -p "$WT_TARGET" "$OVERLAY_UPPER" "$OVERLAY_WORK"
  fuse-overlayfs \
    -o "lowerdir=$MAIN_TARGET,upperdir=$OVERLAY_UPPER,workdir=$OVERLAY_WORK" \
    "$WT_TARGET"
fi

# 3. Print the worktree path (required by Claude Code)
echo "$WORKTREE_DIR"
