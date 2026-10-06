use nd_composer::{ComposerAction, ComposerState, InputEvent, Key, Modifiers, step};

fn ready(text: &str) -> ComposerState {
    ComposerState {
        text: text.into(),
        focused: true,
        send_enabled: true,
        ..Default::default()
    }
}
fn key(key: Key, modifiers: Modifiers) -> InputEvent {
    InputEvent::KeyDown {
        key,
        modifiers,
        held: false,
    }
}

#[test]
fn enter_offers_exact_unicode_text_without_clearing_the_draft() {
    let before = ready("你好🙂\n  保留空格 ");
    let (after, actions) = step(before.clone(), key(Key::Enter, Modifiers::default()));
    assert_eq!(
        actions,
        vec![ComposerAction::Submit {
            text: "你好🙂\n  保留空格 ".into()
        }]
    );
    assert_eq!(after, before);
}

#[test]
fn marked_text_blocks_every_enter_variant_and_button_until_committed() {
    let (composing, _) = step(
        ready("前🙂"),
        InputEvent::Edit {
            text: "前🙂ni".into(),
            composing: true,
        },
    );
    for modifiers in [
        Modifiers::default(),
        Modifiers {
            shift: true,
            ..Default::default()
        },
        Modifiers {
            alt: true,
            ..Default::default()
        },
    ] {
        let (after, actions) = step(composing.clone(), key(Key::Enter, modifiers));
        assert!(actions.is_empty());
        assert_eq!(after.text, "前🙂ni");
    }
    assert!(step(composing.clone(), InputEvent::Submit).1.is_empty());
    let (committed, _) = step(
        composing,
        InputEvent::Edit {
            text: "前🙂你".into(),
            composing: false,
        },
    );
    assert_eq!(
        step(committed, key(Key::Enter, Modifiers::default())).1,
        vec![ComposerAction::Submit {
            text: "前🙂你".into()
        }]
    );
}

#[test]
fn shift_and_alt_enter_insert_at_editor_selection_without_submitting() {
    for modifiers in [
        Modifiers {
            shift: true,
            ..Default::default()
        },
        Modifiers {
            alt: true,
            ..Default::default()
        },
        Modifiers {
            shift: true,
            alt: true,
            ..Default::default()
        },
    ] {
        let (after, actions) = step(ready("甲🙂乙"), key(Key::Enter, modifiers));
        assert_eq!(actions, vec![ComposerAction::InsertNewline]);
        assert_eq!(after.text, "甲🙂乙"); // 编辑器应用选择区替换后以 Edit 回报。
    }
    for modifiers in [
        Modifiers {
            control: true,
            ..Default::default()
        },
        Modifiers {
            platform: true,
            ..Default::default()
        },
    ] {
        assert!(step(ready("甲"), key(Key::Enter, modifiers)).1.is_empty());
    }
}

#[test]
fn escape_cancels_composition_exclusively_then_delegates_the_next_escape() {
    let state = ComposerState {
        composing: true,
        ..ready("前🙂ni")
    };
    let (state, actions) = step(state, key(Key::Escape, Modifiers::default()));
    assert_eq!(actions, vec![ComposerAction::CancelComposition]);
    let (state, _) = step(
        state,
        InputEvent::Edit {
            text: "前🙂".into(),
            composing: false,
        },
    );
    let (state, actions) = step(state, key(Key::Escape, Modifiers::default()));
    assert_eq!(actions, vec![ComposerAction::Escape]);
    assert_eq!(state.text, "前🙂");
}

#[test]
fn unfocused_disabled_blank_and_held_keys_do_not_offer_a_submission() {
    let (blurred, _) = step(ready("草稿"), InputEvent::Focus(false));
    for k in [Key::Enter, Key::Escape] {
        assert!(
            step(blurred.clone(), key(k, Modifiers::default()))
                .1
                .is_empty()
        );
        assert!(
            step(
                ready("草稿"),
                InputEvent::KeyDown {
                    key: k,
                    modifiers: Modifiers::default(),
                    held: true
                }
            )
            .1
            .is_empty()
        );
    }
    let (disabled, _) = step(ready("草稿"), InputEvent::EnableSend(false));
    for state in [disabled.clone(), ready(" \n\t")] {
        assert!(step(state.clone(), InputEvent::Submit).1.is_empty());
        assert!(
            step(state, key(Key::Enter, Modifiers::default()))
                .1
                .is_empty()
        );
    }
    // 不能发送时仍可编辑；按钮不要求编辑器焦点，但共用组词与准入守卫。
    assert_eq!(
        step(
            disabled,
            key(
                Key::Enter,
                Modifiers {
                    alt: true,
                    ..Default::default()
                }
            )
        )
        .1,
        vec![ComposerAction::InsertNewline]
    );
    assert_eq!(
        step(blurred, InputEvent::Submit).1,
        vec![ComposerAction::Submit {
            text: "草稿".into()
        }]
    );
}

#[test]
fn an_attachment_only_draft_can_submit_but_never_during_composition() {
    let mut state = ready("");
    state.has_attachments = true;
    assert_eq!(
        step(state.clone(), InputEvent::Submit).1,
        [ComposerAction::Submit {
            text: String::new()
        }]
    );
    state.composing = true;
    assert!(step(state, InputEvent::Submit).1.is_empty());
}
