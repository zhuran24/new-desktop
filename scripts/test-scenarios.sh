#!/usr/bin/env bash
# Run the real systemd/bwrap scenarios, including the pinned offline Claude CLI.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
branch=$(git branch --show-current)
case "$branch" in
  ticket/*) build_name="ticket-${branch#ticket/}" ;;
  bug/*) build_name="bug-${branch#bug/}" ;;
  review/fixes) build_name=review-fixes ;;
  *) build_name=v1 ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/mnt/wd_external/nd-build/target/$build_name}"
export CARGO_BUILD_JOBS=6
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"
limited() {
  systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- "$@"
}
limited cargo build -p nd-watchdog -p nd-daemon --features scenarios --locked
limited cargo build -p nd-desktop --features nd-desktop/scenarios --locked
export ND_TEST_WATCHDOG="$CARGO_TARGET_DIR/debug/nd-watchdog"
export ND_TEST_DAEMON="$CARGO_TARGET_DIR/debug/nd-daemon"
export ND_TEST_DESKTOP="$CARGO_TARGET_DIR/debug/nd-desktop"
limited cargo test -p nd-daemon -p nd-testkit -p nd-claude --features nd-daemon/scenarios,nd-testkit/scenarios,nd-claude/scenarios --locked "$@"
# 真 Fcitx/Rime + 独立 EIS 键盘；系统设置经私有 KWin repeat_info 进入产品。
escape_output="${ND67_LAB_OUTPUT:-$(mktemp -d /mnt/wd_external/nd-build/tmp/native-escape-XXXXXX)}"
python -B crates/nd-desktop/tests/native_escape.py --bin-dir "$CARGO_TARGET_DIR/debug" --output "$escape_output/default" --reconfigure
python -B crates/nd-desktop/tests/native_escape.py --bin-dir "$CARGO_TARGET_DIR/debug" --output "$escape_output/fast" --repeat-rate 40 --repeat-delay 250
python -B crates/nd-desktop/tests/native_escape.py --bin-dir "$CARGO_TARGET_DIR/debug" --output "$escape_output/slow" --repeat-rate 12 --repeat-delay 900
python -B crates/nd-desktop/tests/native_escape.py --bin-dir "$CARGO_TARGET_DIR/debug" --output "$escape_output/disabled" --repeat-rate 0 --repeat-delay 600
