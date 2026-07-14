#!/usr/bin/env bash
# A miniature CI setup script with a deliberately non-obvious time profile:
# the loop *looks* like the slow part, but the profiler shows the real cost
# sits in two innocuous lines. Run it with:
#
#   bashprof run examples/ci-setup.sh
set -euo pipefail

fetch_deps() {
  local pkg
  for pkg in alpha beta gamma; do
    sleep 0.12 # stand-in for a registry fetch
  done
}

compile_assets() {
  sleep 0.35 # stand-in for a bundler pass
}

warm_cache() {
  local i
  for i in $(seq 1 50); do
    echo "cache-entry-$i" >> "$CACHE_FILE"
  done
}

CACHE_FILE=$(mktemp)
trap 'rm -f "$CACHE_FILE"' EXIT

fetch_deps
compile_assets
warm_cache
echo "setup complete"
