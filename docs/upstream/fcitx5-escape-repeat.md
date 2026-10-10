# Wayland: holding Escape after Rime cancels preedit produces multiple fresh application key presses

Draft GitHub issue for [fcitx/fcitx5](https://github.com/fcitx/fcitx5/issues). Verified on 2026-10-10; not submitted.

## Describe the bug

In a native Wayland GPUI application, type a Rime preedit and hold Escape. Rime consumes the initial Escape to cancel the preedit. Subsequent automatic repeats arrive at the application as release/press pairs. The application therefore treats them as new presses after composition has already ended.

In an isolated reproduction, a 0.9-second hold increased the application's non-composition Escape counter from 0 to 8. The surrounding committed text remained `左🙂右`. The injector sent exactly one Escape press and one release; Fcitx generated the repeats. The application retained focus throughout the hold.

## Environment

CachyOS Linux, kernel `7.2.9-1-cachyos`, KDE Wayland:

| Component | Installed version |
|---|---|
| fcitx5 | `5.1.23-1.1` |
| fcitx5-rime | `5.1.16-1.1` |
| librime | `1:1.17.0-5.1` |
| KWin / Plasma Desktop | `6.7.5-1.1` |
| Qt / KConfig | `6.11.2-3.1` / `6.30.0-1.1` |
| Wayland | `1.26.0-1.1` |
| libei | `1.6.0-1` |
| Application framework | `gpui-pre` / `gpui-pre-linux` `0.3.7` |

Rime schema: Luna Pinyin. The private compositor advertises `zwp_input_method_v1`; the application uses `zwp_text_input_v3`. Keyboard repeat settings, confirmed by protocol logging: 25 Hz, 600 ms delay. Package versions were checked with `pacman -Q`; framework versions with Cargo.lock and the installed registry sources.

## Steps to reproduce

1. Start a private KWin virtual display with Fcitx 5 as its input method, a private D-Bus session, and fresh XDG/HOME directories. Enable Rime Luna Pinyin.
2. Open a native GPUI textarea with a counter for Escape actions outside composition. Put `左🙂右` in it and place the caret before `右`.
3. Type `nihao` to start a preedit.
4. Press Escape once, hold it for 0.9 seconds, then release it.
5. Observe the composition state, counter, and `WAYLAND_DEBUG=client` log. Confirm that the keyboard injector supplied only one physical press/release pair.

The automated reproducer lives in [New Desktop](https://github.com/zhuran24/new-desktop), branch `bug/67`: `crates/nd-desktop/tests/native_escape.py` and `private_keyboard.c`. Build `nd-desktop` with the `scenarios` feature, then run:

```sh
python -B crates/nd-desktop/tests/native_escape.py \
  --bin-dir "$CARGO_TARGET_DIR/debug" --output /path/to/escape-evidence
```

It creates its own KWin, Fcitx/Rime, D-Bus, and EIS keyboard inside an offline sandbox. It never sends events to the user's display. Dependencies and isolation requirements are documented in `docs/verification/bug-67.md`. The application now has a cadence workaround; reproducing the original failure requires the pre-fix revision `9e38b5dc23f3360c143a40254d4e1907fd9c80f1`.

## Actual and expected behavior

Actual: after preedit cancellation, repeats become fresh non-composition Escape actions. They can interrupt an active agent turn or repeatedly open and close a rewind menu.

Expected: applications should be able to distinguish automatic repetition from a new press. In particular, an Escape hold used to cancel preedit should not silently turn into a series of independent application actions. A deliberate release followed by another press must remain usable.

## Source evidence and protocol constraints

Tag `5.1.23` resolves to commit `27cca6e0239938b2d53937061d0980c087856b22` (verified using GitHub's tag and contents APIs):

- [V1 repeat(), lines 238–263](https://github.com/fcitx/fcitx5/blob/27cca6e0239938b2d53937061d0980c087856b22/src/frontend/waylandim/waylandimserver.cpp#L238): Fcitx marks its internal key as a repeat, forwards a release, and forwards a press if the repeat is unhandled. Both use the original key timestamp. [keyCallback(), lines 497–524](https://github.com/fcitx/fcitx5/blob/27cca6e0239938b2d53937061d0980c087856b22/src/frontend/waylandim/waylandimserver.cpp#L497) records that original time.
- [V2 repeat(), lines 287–308](https://github.com/fcitx/fcitx5/blob/27cca6e0239938b2d53937061d0980c087856b22/src/frontend/waylandim/waylandimserverv2.cpp#L287) uses the same release/press strategy. This is static evidence; the runtime reproduction exercises V1 only.

KWin's forwarding also loses the supplied timestamp; a separate draft describes that issue. Fixing timestamp propagation alone does not restore a physical release/repeat identity at the GPUI public interface.

Legacy `wl_keyboard` clients cannot receive the version-10 repeated state, and duplicate pressed events without a logical release violate [Wayland's key-state rules](https://gitlab.freedesktop.org/wayland/wayland/-/blob/1.26.0/protocol/wayland.xml). This report does not propose simply deleting the synthetic release. Please investigate preserving repeat provenance, or preventing consumed Escape holds from leaking as new actions, within the supported protocol's constraints.

## Evidence

The reproducer saves `input.jsonl` (EIS injection receipts), `lab.wayland.log` (application protocol), `result.json` (visible textarea/counter state), and `cleanup.json` (private unit and directory cleanup). Full reproduction, counterexamples, and source verification are in `docs/verification/bug-67.md`. This is a private virtual-display reproduction, not a physical-keyboard acceptance result.
