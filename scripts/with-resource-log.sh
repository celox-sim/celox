#!/usr/bin/env bash
# Run a command and print the machine's memory, disk and largest process
# every minute to stderr, so a CI job whose runner is starved leaves
# evidence of what grew. Usage: scripts/with-resource-log.sh <command...>
set -euo pipefail

log_resources() {
  while sleep 60; do
    local memory disk top
    memory=$(free -m | awk 'NR == 2 { print $3 "/" $2 " MiB used, " $7 " MiB available" }')
    disk=$(df -h . | awk 'NR == 2 { print $4 " disk free" }')
    top=$(ps -eo rss=,comm= --sort=-rss | awk 'NR == 1 { printf "%s %d MiB", $2, $1 / 1024 }')
    echo "resources: $memory; $disk; largest: $top" >&2
  done
}

log_resources &
logger=$!
trap 'kill "$logger" 2>/dev/null || true' EXIT
"$@"
