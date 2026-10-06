//! 合成组件检查，不是豆包/Rime 或实际上屏性能验收。
use gpui::{
    EntityInputHandler, KeyDownEvent, KeyUpEvent, Keystroke, PlatformInput, TestAppContext,
    VisualTestContext,
};
use nd_composer::ComposerAction;
use nd_desktop::composer::{Composer, ComposerEvent};
use nd_view_model::{Theme, ThemeMode};
use std::{cell::RefCell, rc::Rc};

fn key(cx: &mut VisualTestContext, name: &str, release: bool) {
    cx.update(|window, cx| {
        let keystroke = Keystroke::parse(name).unwrap();
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
        if release {
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        }
    });
    cx.run_until_parked();
}

#[gpui::test]
fn alt_enter_replaces_the_selected_unicode_text_with_one_newline(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (composer, cx) =
        cx.add_window_view(|window, cx| Composer::new(Theme::builtin(ThemeMode::Dark), window, cx));
    let input = composer.read_with(cx, |c, _| c.editor().clone());
    input.update_in(cx, |input, window, cx| {
        input.set_value("甲🙂乙", window, cx);
        input.set_selected_range(3..7, cx); // Kit 的选择区是 UTF-8，选中 emoji。
        input.focus(window, cx);
    });
    key(cx, "alt-enter", true);
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_string()),
        "甲\n乙"
    );
}

#[gpui::test]
fn holding_enter_offers_once_until_key_release(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (composer, cx) =
        cx.add_window_view(|window, cx| Composer::new(Theme::builtin(ThemeMode::Dark), window, cx));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let observed = actions.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&composer, move |_, event, _| {
            if let ComposerEvent::Action(action) = event {
                observed.borrow_mut().push(action.clone());
            }
        })
    });
    composer.update_in(cx, |composer, window, cx| {
        composer.set_send_enabled(true, cx);
        composer.editor().update(cx, |input, cx| {
            input.set_value("发送一次", window, cx);
            input.focus(window, cx);
        });
    });
    key(cx, "enter", false);
    key(cx, "enter", true);
    assert_eq!(
        *actions.borrow(),
        vec![ComposerAction::Submit {
            text: "发送一次".into()
        }]
    );
    key(cx, "enter", true);
    assert_eq!(actions.borrow().len(), 2);
}

#[gpui::test]
fn composition_is_guarded_before_kit_actions_and_escape_removes_only_the_marked_utf16_range(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let (composer, cx) =
        cx.add_window_view(|window, cx| Composer::new(Theme::builtin(ThemeMode::Dark), window, cx));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let observed = actions.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&composer, move |_, event, _| {
            if let ComposerEvent::Action(action) = event {
                observed.borrow_mut().push(action.clone());
            }
        })
    });
    let input = composer.read_with(cx, |c, _| c.editor().clone());
    composer.update(cx, |c, cx| c.set_send_enabled(true, cx));
    input.update_in(cx, |input, window, cx| {
        input.set_value("前🙂后", window, cx);
        input.replace_and_mark_text_in_range(Some(3..3), "ni", Some(2..2), window, cx);
        input.focus(window, cx);
    });
    for name in ["enter", "shift-enter", "alt-enter", "ctrl-enter"] {
        key(cx, name, true);
        assert_eq!(
            input.read_with(cx, |s, _| s.value().to_string()),
            "前🙂ni后"
        );
        assert!(
            composer
                .update_in(cx, |c, w, cx| c.snapshot(w, cx))
                .composing
        );
    }
    composer.update_in(cx, |c, w, cx| c.submit(w, cx));
    assert!(actions.borrow().is_empty());
    key(cx, "escape", false);
    key(cx, "escape", true); // 同一次按住 Esc 不能取消后又停回合。
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "前🙂后");
    assert!(
        !composer
            .update_in(cx, |c, w, cx| c.snapshot(w, cx))
            .composing
    );
    assert!(actions.borrow().is_empty());
    key(cx, "escape", true);
    assert_eq!(*actions.borrow(), vec![ComposerAction::Escape]);
    actions.borrow_mut().clear();
    // 固定 Kit 把空 preedit 当结束组词；以 trait 的 marked range 为准。
    input.update_in(cx, |input, w, cx| {
        input.replace_and_mark_text_in_range(Some(3..3), "", Some(0..0), w, cx)
    });
    assert!(
        !composer
            .update_in(cx, |c, w, cx| c.snapshot(w, cx))
            .composing
    );
    input.update_in(cx, |input, w, cx| {
        input.replace_and_mark_text_in_range(Some(3..3), "ni", Some(2..2), w, cx)
    });
    key(cx, "enter", true);
    assert!(actions.borrow().is_empty());
    input.update_in(cx, |input, w, cx| {
        input.replace_text_in_range(None, "你", w, cx)
    });
    key(cx, "enter", true);
    assert_eq!(
        *actions.borrow(),
        vec![ComposerAction::Submit {
            text: "前🙂你后".into()
        }]
    );
}

#[gpui::test]
fn send_button_preserves_composition_and_disabled_or_unfocused_keys_cannot_send(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let (composer, cx) =
        cx.add_window_view(|window, cx| Composer::new(Theme::builtin(ThemeMode::Dark), window, cx));
    let actions = Rc::new(RefCell::new(Vec::new()));
    let observed = actions.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&composer, move |_, event, _| {
            if let ComposerEvent::Action(action) = event {
                observed.borrow_mut().push(action.clone());
            }
        })
    });
    let input = composer.read_with(cx, |c, _| c.editor().clone());
    input.update_in(cx, |i, w, cx| {
        i.set_value("草稿", w, cx);
        i.focus(w, cx);
    });
    key(cx, "enter", true);
    composer.update_in(cx, |c, w, cx| c.submit(w, cx));
    assert!(actions.borrow().is_empty());
    composer.update(cx, |c, cx| c.set_send_enabled(true, cx));
    cx.update(|w, cx| w.blur(cx));
    key(cx, "enter", true);
    key(cx, "escape", true);
    assert!(actions.borrow().is_empty());
    input.update_in(cx, |i, w, cx| {
        i.focus(w, cx);
        i.set_selected_range(6..6, cx);
        i.replace_and_mark_text_in_range(None, "ni", Some(2..2), w, cx);
    });
    cx.run_until_parked();
    let button = cx.debug_bounds("composer-submit").unwrap().center();
    cx.simulate_click(button, gpui::Modifiers::default());
    let snapshot = composer.update_in(cx, |c, w, cx| c.snapshot(w, cx));
    assert!(snapshot.focused && snapshot.composing);
    assert!(actions.borrow().is_empty());
    input.update_in(cx, |i, w, cx| i.replace_text_in_range(None, "你", w, cx));
    cx.run_until_parked();
    cx.simulate_click(button, gpui::Modifiers::default());
    assert_eq!(
        *actions.borrow(),
        vec![ComposerAction::Submit {
            text: "草稿你".into()
        }]
    );
    assert_eq!(input.read_with(cx, |i, _| i.value().to_string()), "草稿你");
}
