#!/usr/bin/env bash
# Run the real systemd/bwrap scenarios, including the pinned offline Claude CLI.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
branch=$(git branch --show-current)
case "$branch" in
  ticket/*) build_name="ticket-${branch#ticket/}" ;;
  *) build_name=v1 ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/mnt/wd_external/nd-build/target/$build_name}"
export CARGO_BUILD_JOBS=6
limited() {
  systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- "$@"
}
limited cargo build -p nd-daemon --features scenarios --locked
export ND_TEST_DAEMON="$CARGO_TARGET_DIR/debug/nd-daemon"
limited cargo test -p nd-daemon -p nd-testkit --features nd-daemon/scenarios,nd-testkit/scenarios --locked "$@"
