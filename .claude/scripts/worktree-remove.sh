#!/usr/bin/env bash
set -euo pipefail

# WorktreeRemove hook for Claude Code
# Unmounts the overlayfs and removes the git worktree.
#
# Input (JSON on stdin): { "worktree_path": "...", "cwd": "...", ... }

INPUT=$(cat)
WORKTREE_DIR=$(echo "$INPUT" | jq -r '.worktree_path')
MAIN_DIR=$(echo "$INPUT" | jq -r '.cwd')
WT_TARGET="$WORKTREE_DIR/target"

# 1. Unmount overlayfs if mounted
if mountpoint -q "$WT_TARGET" 2>/dev/null; then
  fusermount3 -u "$WT_TARGET" || fusermount -u "$WT_TARGET" || true
fi

# 2. Extract branch name before removing worktree
BRANCH=$(git -C "$WORKTREE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || true)

# 3. Remove the git worktree
git -C "$MAIN_DIR" worktree remove --force "$WORKTREE_DIR" 2>/dev/null || rm -rf "$WORKTREE_DIR"

# 4. Clean up the temporary branch
if [ -n "$BRANCH" ] && [[ "$BRANCH" == claude/* ]]; then
  git -C "$MAIN_DIR" branch -D "$BRANCH" 2>/dev/null || true
fi
