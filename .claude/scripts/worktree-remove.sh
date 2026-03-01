#!/usr/bin/env bash
set -euo pipefail

# WorktreeRemove hook for Claude Code
# Unmounts all overlayfs layers and removes the git worktree.
#
# Input (JSON on stdin): { "worktree_path": "...", "cwd": "...", ... }

INPUT=$(cat)
WORKTREE_DIR=$(echo "$INPUT" | jq -r '.worktree_path')
MAIN_DIR=$(echo "$INPUT" | jq -r '.cwd')

# 1. Unmount all overlayfs mounts within the worktree (reverse order for safety)
mount | grep -F " on $WORKTREE_DIR/" | awk '{print $3}' | sort -r | while read -r mnt; do
  fusermount3 -u "$mnt" 2>/dev/null || fusermount -u "$mnt" 2>/dev/null || true
done

# 2. Extract branch name before removing worktree
BRANCH=$(git -C "$WORKTREE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || true)

# 3. Remove the git worktree
git -C "$MAIN_DIR" worktree remove --force "$WORKTREE_DIR" 2>/dev/null || rm -rf "$WORKTREE_DIR"

# 4. Clean up the temporary branch
if [ -n "$BRANCH" ] && [[ "$BRANCH" == claude/* ]]; then
  git -C "$MAIN_DIR" branch -D "$BRANCH" 2>/dev/null || true
fi
