#!/usr/bin/env bash
# One-time setup: create placeholder packages on npm so that trusted publishing
# (OIDC) can be configured for each package on npmjs.com.
#
# Prerequisites:
#   - npm login (you must be logged in to npm with publish rights to @celox-sim)
#
# After running this script:
#   1. Go to https://www.npmjs.com/package/<name>/access for each package
#   2. Add a trusted publisher:
#        Provider:   GitHub Actions
#        Owner:      celox-sim
#        Repository: celox
#        Workflow:   ci-napi.yml
set -euo pipefail

PACKAGES=(
  "@celox-sim/celox"
  "@celox-sim/vite-plugin"
  "@celox-sim/celox-napi"
  "@celox-sim/celox-napi-linux-x64-gnu"
  "@celox-sim/celox-napi-linux-arm64-gnu"
  "@celox-sim/celox-napi-linux-x64-musl"
  "@celox-sim/celox-napi-linux-arm64-musl"
  "@celox-sim/celox-napi-darwin-x64"
  "@celox-sim/celox-napi-darwin-arm64"
  "@celox-sim/celox-napi-win32-x64-msvc"
)

for pkg in "${PACKAGES[@]}"; do
  echo "=== Setting up $pkg ==="
  npx setup-npm-trusted-publish "$pkg"
  echo ""
done

echo "Done. Now configure trusted publishers on npmjs.com for each package:"
for pkg in "${PACKAGES[@]}"; do
  echo "  https://www.npmjs.com/package/${pkg}/access"
done
