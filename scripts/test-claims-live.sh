#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
branch=$(git branch --show-current)
case "$branch" in
  ticket/*) build_name="ticket-${branch#ticket/}" ;;
  *) build_name=v1 ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/mnt/wd_external/nd-build/target/$build_name}"
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo build -p nd-claims --example registry_probe --locked
evidence=${ND_CLAIMS_EVIDENCE:-/mnt/wd_external/nd-build/tmp/claims-live-$(date +%Y%m%dT%H%M%S)-$$}
python crates/nd-claims/tests/cli/generate.py "$evidence" "$CARGO_TARGET_DIR/debug/examples/registry_probe"
