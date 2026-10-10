#!/usr/bin/env bash
# Release 产品、私有 KWin、真 Rime。默认三种状态各 60 秒；不操作日常桌面。
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
branch=$(git branch --show-current)
case "$branch" in
  bug/*) build_name="bug-${branch#bug/}" ;;
  ticket/*) build_name="ticket-${branch#ticket/}" ;;
  *) build_name=v1 ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/mnt/wd_external/nd-build/target/$build_name}"
export CARGO_BUILD_JOBS=6
limited() {
  systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- "$@"
}
limited cargo build --release --locked -p nd-desktop -p nd-daemon -p nd-watchdog
export ND_TEST_DAEMON="$CARGO_TARGET_DIR/release/nd-daemon"
export ND_TEST_WATCHDOG="$CARGO_TARGET_DIR/release/nd-watchdog"
export ND_TEST_DESKTOP="$CARGO_TARGET_DIR/release/nd-desktop"
export ND_NATIVE_CPU_OUTPUT="${ND_NATIVE_CPU_OUTPUT:-$(mktemp -d /mnt/wd_external/nd-build/tmp/nd-idle-cpu-XXXXXXXX)}"
limited cargo test --release --locked -p nd-daemon --features scenarios --test sessions \
  focused_product_idle_cpu_stays_below_one_percent_with_real_rime -- --ignored --exact --nocapture
