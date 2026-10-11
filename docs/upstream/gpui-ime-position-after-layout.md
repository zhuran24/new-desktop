# Wayland IME candidate rectangle stays stale after textarea layout changes

Draft for GPUI / GPUI Kit maintainers. Verified on 2026-10-10; not submitted.

## Environment

- CachyOS Linux, KWin `6.7.5-1.1`, Qt `6.11.2-3.1`, Wayland `1.26.0-1.1`.
- Real Fcitx `5.1.23-1.1`, fcitx5-rime `5.1.16-1.1`, librime `1:1.17.0-5.1`, Luna Pinyin, ClassicUI.
- `gpui-pre` / `gpui-pre-linux` `0.3.7`, GPUI Kit revision `4c7f1350331562436df868c55ac33bebc4c6406c`.
- Private virtual KWin with two 1400 × 1000 outputs, independent HOME/XDG/D-Bus, and network isolation. No events or test windows were sent to the user's display.

The inspected GPUI files match both their published 0.3.7 crate archives and Zed snapshot `1a28cff4b409169bac058bca40dfbfeb7621d19b` byte for byte. Later snapshots have not been tested.

## Reproduction

1. Create a GPUI Kit `TextareaState` / `Textarea` anchored near the bottom of a resizable window. Focus it and select Rime Luna Pinyin in Chinese mode.
2. Type `nihao` to retain a real preedit (`ni hao`) with visible candidates.
3. Reduce the window height by 40 logical pixels, without pressing another editing key.
4. Compare the candidate panel with the visible preedit and inspect the client's `zwp_text_input_v3.set_cursor_rectangle` requests.

The automated product reproducer is [New Desktop #73](https://github.com/zhuran24/new-desktop/issues/73): `crates/nd-desktop/tests/native_ime_position.py`, invoked by the `real_rime_candidates_follow_window_resize_and_cross_output_drag` scenario in `crates/nd-daemon/tests/sessions.rs`.

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-73
export CARGO_BUILD_JOBS=6
export ND73_NATIVE_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-73-recheck
bash scripts/test-scenarios.sh \
  real_rime_candidates_follow_window_resize_and_cross_output_drag -- --exact --nocapture
```

The parent scenario creates a real daemon/CLI session against an offline model endpoint. The native driver independently starts KWin and Fcitx/Rime, sends native keyboard/pointer events, and captures compositor screenshots. Its glyph oracle uses an identical-size screenshot after cancelling the preedit, masks the actual candidate panel, and never uses the reported caret rectangle as the expected location. It also checks that geometry stays unchanged across each capture.

New Desktop's current application contains the workaround below. To reproduce the failure, use baseline `48e9bc20925119f5ac3aebc7c7322cd019257e0e` with the test files, or remove only the workaround's invalidation call. The final test was rerun with that call removed and failed again.

## Observed and expected results

Before resize, the candidate's top is 3 px below the preedit glyphs. After shrinking, the glyphs move up by 40 px but the candidate remains at its original position, leaving a 39 px gap. The client sends only the original rectangle `(291, 649, 0, 18)`; there is no resize update.

With application-side coordinate invalidation, the client sends `(291, 609, 0, 18)` after shrinking. Candidate-to-glyph separation remains 3 px. Enlarging, dragging the titlebar across both virtual outputs, and resizing a scrolled Chinese/emoji draft so its content wraps also pass.

Expected behavior: a focused editor's candidate rectangle follows its painted layout while composition remains active, even when there is no new editing key or input-method event.

## Source diagnosis

- [GPUI `Window::draw`](https://github.com/zed-industries/zed/blob/1a28cff4b409169bac058bca40dfbfeb7621d19b/crates/gpui/src/window.rs#L3257) installs the current input handler and updates input configuration/focus, but does not refresh the IME rectangle after layout/paint.
- [The Wayland text-input Done handler](https://github.com/zed-industries/zed/blob/1a28cff4b409169bac058bca40dfbfeb7621d19b/crates/gpui_linux/src/linux/wayland/client.rs#L2121) queries the IME area after an input-method event. A resize alone does not enter this path.
- [Kit's input element paint](https://github.com/longbridge/gpui-kit/blob/4c7f1350331562436df868c55ac33bebc4c6406c/crates/base/src/input/base/element.rs#L3273) stores the current layout/bounds and notifies the editor when geometry changes.
- [GPUI `Window::invalidate_character_coordinates`](https://github.com/zed-industries/zed/blob/1a28cff4b409169bac058bca40dfbfeb7621d19b/crates/gpui/src/window.rs#L6441) already exposes the required refresh. [PlatformInputHandler](https://github.com/zed-industries/zed/blob/1a28cff4b409169bac058bca40dfbfeb7621d19b/crates/gpui/src/platform.rs#L1777) derives the candidate anchor from the marked range and caret's current visual line. [The Wayland sender](https://github.com/zed-industries/zed/blob/1a28cff4b409169bac058bca40dfbfeb7621d19b/crates/gpui_linux/src/linux/wayland/client.rs#L163) commits changed rectangles and deduplicates unchanged rectangles.

This is a missing automatic refresh between painted editor geometry and GPUI's platform IME state. The application also did not explicitly invalidate those coordinates. The failure does not establish a KWin or Rime positioning defect: the original client never supplies the new rectangle, and the same compositor/input method places it correctly when supplied.

## Application workaround and possible upstream change

New Desktop observes its Kit editor. If that editor is focused, each editor notification calls `window.invalidate_character_coordinates()`. Kit's notification after a geometry change makes the next callback use the newly painted layout. The application keeps the normal GPUI input handler, marked range, focus, and Wayland text-input lifecycle; it does not calculate or offset the candidate position itself.

The workaround is in the product adapter and does not patch GPUI, Kit, or the Cargo registry. An upstream change is not required for this product fix. A useful upstream improvement would invalidate candidate coordinates after focused editor geometry changes, or explicitly wire/document that responsibility in Kit's input element. It should retain changed-rectangle deduplication and avoid a perpetual paint/notify/commit loop.

## Evidence and limits

Local evidence: `/mnt/wd_external/nd-build/tmp/bug-73/`.

- `red2/` and `red-final-oracle/`: failing screenshots, measurements, native event receipts, and Wayland logs.
- `matrix2/`: the corrected independent oracle, successful resize/drag/reflow screenshots and protocol updates.
- `source-provenance.json`, `upstream-window.rs`, `upstream-client.rs`, `upstream-platform.rs`: pinned source identity and retrieved primary sources.
- [Product verification](../verification/bug-73.md): final acceptance commands, cleanup evidence, and physical-desktop boundary.

Both outputs in this reproduction are virtual and use scale 1. Physical displays, fractional scaling, and the user's daily input-method setup remain separate owner-scheduled checks.
