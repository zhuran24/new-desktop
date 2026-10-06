//! Synthetic component checks; no compositor keyboard injection or real IME.
use super::*;

pub(super) fn start(window: &mut Window, cx: &mut Context<Lab>) {
    // This optional driver is forbidden in the owner's desktop session.
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(
        display.starts_with("ime-lab-") && runtime.starts_with("/tmp/ime-lab-smoke-"),
        "IME_LAB_SCENARIO=check requires scripts/smoke.py's private compositor"
    );
    cx.spawn_in(window, async move |lab, cx| {
        cx.background_executor().timer(Duration::from_millis(700)).await;
        for step in 0..13 {
            // Let action subscriptions and paint complete between assertions.
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let key = lab.update_in(cx, |lab, window, cx| {
                lab.log.emit(json!({"event":"scenario_step", "step":step,
                    "source":"synthetic component driver"}));
                match step {
                    0 => {
                        lab.input.update(cx, |input, cx| {
                            input.replace_text_in_range(None, "测试🙂", window, cx);
                        });
                        None
                    }
                    1 => {
                        assert_eq!(lab.input.read(cx).value().as_ref(), "测试🙂");
                        Some("shift-enter")
                    }
                    2 => {
                        assert_eq!(lab.input.read(cx).value().as_ref(), "测试🙂\n");
                        assert!(lab.messages.is_empty());
                        Some("ctrl-enter")
                    }
                    3 => {
                        assert_eq!(lab.input.read(cx).value().as_ref(), "测试🙂\n");
                        assert!(lab.messages.is_empty());
                        Some("enter")
                    }
                    4 => {
                        assert_eq!(lab.messages, vec!["测试🙂\n"]);
                        assert_eq!(lab.input.read(cx).value().as_ref(), "");
                        lab.input.update(cx, |input, cx| {
                            input.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                        });
                        None
                    }
                    5 => {
                        assert_eq!(lab.input.read(cx).ime_lab_snapshot()["has_preedit"], true);
                        Some("enter")
                    }
                    6 => {
                        if lab.guard {
                            assert_eq!(lab.messages.len(), 1);
                            assert_eq!(lab.input.read(cx).value().as_ref(), "ni");
                            assert_eq!(lab.input.read(cx).ime_lab_snapshot()["has_preedit"], true);
                        } else {
                            // Baselines intentionally expose the upstream bug.
                            assert_eq!(lab.messages, vec!["测试🙂\n", "ni"]);
                            assert_eq!(lab.input.read(cx).value().as_ref(), "");
                        }
                        lab.input.update(cx, |input, cx| {
                            input.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                            input.replace_text_in_range(None, "你", window, cx);
                        });
                        None
                    }
                    7 => {
                        assert_eq!(lab.input.read(cx).value().as_ref(), "你");
                        assert_eq!(lab.input.read(cx).ime_lab_snapshot()["has_preedit"], false);
                        Some("enter")
                    }
                    8 => {
                        assert_eq!(lab.messages.last().map(String::as_str), Some("你"));
                        assert_eq!(lab.input.read(cx).value().as_ref(), "");
                        lab.input.update(cx, |input, cx| {
                            input.replace_and_mark_text_in_range(None, "取消", Some(0..2), window, cx);
                            input.unmark_text(window, cx);
                        });
                        window.blur(cx);
                        None
                    }
                    9 => {
                        lab.input.update(cx, |input, cx| input.focus(window, cx));
                        None
                    }
                    10 => Some("ctrl-enter"),
                    11 => {
                        assert_eq!(lab.input.read(cx).value().as_ref(), "取消");
                        lab.log.emit(json!({"event":"scenario_pass", "guard":lab.guard,
                            "checks":["unicode commit", "shift-enter newline", "ctrl-enter no send",
                                "enter send and clear", "preedit send guard comparison", "send after commit",
                                "unmark", "blur and focus"],
                            "source":"synthetic component driver; not a real IME test"}));
                        None
                    }
                    _ => None,
                }
            }).expect("scenario window alive");
            if let Some(key) = key {
                // Dispatch outside the Lab borrow: event listeners borrow Lab.
                cx.update(|window, cx| {
                    let key = Keystroke::parse(key).expect("scenario keystroke");
                    // dispatch_keystroke synthesizes a second character-input
                    // callback for Enter. Here we need only the key/action path;
                    // composition and commit callbacks are driven explicitly.
                    window.dispatch_event(
                        PlatformInput::KeyDown(KeyDownEvent {
                            keystroke: key.clone(),
                            is_held: false,
                            prefer_character_input: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke: key }), cx);
                }).expect("scenario dispatch");
            }
        }
    }).detach();
}
