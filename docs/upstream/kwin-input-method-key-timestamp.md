# InputMethod::key discards the input-method timestamp when forwarding keyboard events

Draft for [bugs.kde.org](https://bugs.kde.org/). Verified on 2026-10-10; not submitted.

**Product:** kwin  
**Component:** input  
**Version:** 6.7.5  
**Platform:** CachyOS Linux, Wayland  
**Suggested severity:** normal  
**Summary:** Input-method V1 forwarded keyboard events have time=0 in a private KWin reproduction

## SUMMARY

KWin receives a timestamp in the input-method V1 key request, but its forwarding function does not use it to update the seat timestamp. A native Wayland GPUI client consequently receives timestamp 0 for all forwarded Escape press/release events in the reproduction below.

This was verified with an independent EIS keyboard, not KWin fake-input: one Escape press and one release at the compositor input boundary produce multiple forwarded application press/release pairs when Rime/Fcitx repeats the held key. All their application-side timestamps are 0. In a control experiment, a genuinely released and re-pressed Escape also arrives with timestamp 0. The application retains focus.

## STEPS TO REPRODUCE

1. Start `kwin_wayland --virtual` on a private socket, with a fresh HOME/XDG configuration, a private D-Bus session, and `--inputmethod fcitx5`. Enable Rime Luna Pinyin; use repeat rate 25 Hz and delay 600 ms.
2. Start a native GPUI 0.3.7 textarea client with `WAYLAND_DEBUG=client`. Check that the compositor advertises `zwp_input_method_v1` and that the client enables text-input V3.
3. Put `左🙂右` in the editor, place the caret before `右`, and type `nihao` to create a preedit.
4. Through an EIS keyboard, press Escape once, hold it for 0.9 seconds, then release it. Record the injection receipts and client-side `wl_keyboard.key` messages.
5. As a separate control, short-press Escape to cancel a preedit, release it, wait approximately 565 ms, and short-press it again. Inspect the forwarded key timestamps.

An automated private-display reproducer is provided by [New Desktop](https://github.com/zhuran24/new-desktop), branch `bug/67`, in `crates/nd-desktop/tests/native_escape.py` and `private_keyboard.c`:

```sh
cargo build --locked -p nd-desktop --features scenarios
python -B crates/nd-desktop/tests/native_escape.py \
  --bin-dir "$CARGO_TARGET_DIR/debug" --output /path/to/escape-evidence
```

The script deploys Rime, creates isolated KWin/Fcitx/D-Bus instances and an EIS keyboard, checks the active window PID throughout each hold, and tears down its private units. It does not inject into the user's display. The current application has a cadence workaround; the protocol trace still demonstrates the timestamp loss. Revision `9e38b5dc23f3360c143a40254d4e1907fd9c80f1` demonstrates the original application failure without that workaround.

## OBSERVED RESULT

The application's forwarded Escape presses and releases all carry timestamp 0. Physical repeat and deliberate re-press controls both show the same value. Before the application workaround, holding Escape after cancelling preedit increased its non-composition Escape counter from 0 to 8.

Excerpt from the application-side trace (key arguments are serial, time, key, state; key 1 is Escape):

```text
[11:12:30.899718] wl_keyboard#76.key(73, 0, 1, 0)
[11:12:30.899739] wl_keyboard#76.key(74, 0, 1, 1)
[11:12:30.939715] wl_keyboard#76.key(77, 0, 1, 0)
[11:12:30.939733] wl_keyboard#76.key(78, 0, 1, 1)
```

## EXPECTED RESULT

Forwarded keys should preserve the input method's provided time in the correct units and time domain, rather than using an unrelated or stale seat timestamp. Correct timestamps would preserve available event provenance and make traces and application-side handling more reliable.

This does not imply that KWin alone causes the synthetic release/press pairs: Fcitx generates those pairs. Preserving timestamps also does not by itself supply physical key identity to GPUI. Those are separate layers of the observed behavior.

## SOFTWARE/OS VERSIONS

Verified installed packages: KWin / Plasma Desktop / Plasma Workspace `6.7.5-1.1`, Qt `6.11.2-3.1`, KConfig `6.30.0-1.1`, fcitx5 `5.1.23-1.1`, fcitx5-rime `5.1.16-1.1`, librime `1:1.17.0-5.1`, Wayland `1.26.0-1.1`, libei `1.6.0-1`. Kernel: `7.2.9-1-cachyos`. Application dependencies: `gpui-pre` and `gpui-pre-linux` `0.3.7`, confirmed against Cargo.lock and registry source.

## ADDITIONAL INFORMATION: SOURCE EVIDENCE

KWin tag `v6.7.5` resolves to commit `ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7`, verified through GitHub's tag/contents APIs:

- [InputMethod::key, src/inputmethod.cpp:715–730](https://github.com/KDE/kwin/blob/ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7/src/inputmethod.cpp#L715) accepts `time` but forwards the key through the seat without setting its timestamp.
- [SeatInterface::notifyKeyboardKey, src/wayland/seat.cpp:907–913](https://github.com/KDE/kwin/blob/ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7/src/wayland/seat.cpp#L907) calls the keyboard's send function. [setTimestamp, lines 463–466](https://github.com/KDE/kwin/blob/ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7/src/wayland/seat.cpp#L463) converts microseconds to milliseconds.
- [KeyboardInterface::sendKey, src/wayland/keyboard.cpp:197–218](https://github.com/KDE/kwin/blob/ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7/src/wayland/keyboard.cpp#L197) sends the seat's timestamp to the client.

These functions explain the loss of the supplied time. The runtime observation of zero is specific to this private environment; this report does not claim all input-method keys always have time 0 in every desktop session. Installed `libkwin.so.6.7.5` disassembly independently confirmed that the forwarding function does not retain/use its time argument.

A companion [Fcitx draft](fcitx5-escape-repeat.md) documents repeat forwarding. The reproducer emits injection receipts, client protocol logs, observed editor state, and cleanup records. Full evidence and the physical-desktop verification boundary are documented in `docs/verification/bug-67.md` in New Desktop.
