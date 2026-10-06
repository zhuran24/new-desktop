//! 不依赖界面框架的输入框决策。编辑器负责文本、选择区与 UTF-16 转换。

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposerState {
    pub text: String,
    pub composing: bool,
    pub focused: bool,
    pub send_enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub platform: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// composing 来自 marked range 的存在性，空范围也算组词。
    Edit {
        text: String,
        composing: bool,
    },
    Focus(bool),
    EnableSend(bool),
    KeyDown {
        key: Key,
        modifiers: Modifiers,
        held: bool,
    },
    Submit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerAction {
    /// 只表达意图；宿主接受后才清空，失败或交付不明时保留正文。
    Submit {
        text: String,
    },
    InsertNewline,
    CancelComposition,
    /// 本次 Esc 没有取消组词，交给会话能力处理一次。
    Escape,
}

pub fn step(mut state: ComposerState, event: InputEvent) -> (ComposerState, Vec<ComposerAction>) {
    let submit = |state: &ComposerState| {
        if state.send_enabled && !state.composing && !state.text.trim().is_empty() {
            vec![ComposerAction::Submit {
                text: state.text.clone(),
            }]
        } else {
            vec![]
        }
    };
    let actions = match event {
        InputEvent::Focus(focused) => {
            state.focused = focused;
            vec![]
        }
        InputEvent::EnableSend(enabled) => {
            state.send_enabled = enabled;
            vec![]
        }
        InputEvent::Edit { text, composing } => {
            state.text = text;
            state.composing = composing;
            vec![]
        }
        InputEvent::KeyDown { held, .. } if held || !state.focused => vec![],
        InputEvent::KeyDown {
            key: Key::Escape, ..
        } => vec![if state.composing {
            ComposerAction::CancelComposition
        } else {
            ComposerAction::Escape
        }],
        InputEvent::KeyDown {
            key: Key::Enter,
            modifiers,
            ..
        } if !state.composing => {
            if modifiers.control || modifiers.platform {
                vec![]
            } else if modifiers.shift || modifiers.alt {
                vec![ComposerAction::InsertNewline]
            } else {
                submit(&state)
            }
        }
        InputEvent::Submit => submit(&state),
        _ => vec![],
    };
    (state, actions)
}
