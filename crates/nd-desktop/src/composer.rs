//! 输入法与编辑交给固定版 Kit，输入意图只从 nd-composer 产生。
use gpui_kit::component::input::{InputEvent as KitEvent, Textarea, TextareaState};
use gpui_kit::*;
use nd_composer::{ComposerAction, ComposerState, InputEvent, Key, Modifiers, step};
use nd_view_model::Theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    Changed(ComposerState),
    Action(ComposerAction),
}

/// 一个输入框实体对应一个编辑缓冲；宿主保持 Entity 和订阅的生命周期。
/// 不连接后端；Submit/Escape 由会话能力经同步副本处理。
pub struct Composer {
    input: Entity<TextareaState>,
    theme: Theme,
    send_enabled: bool,
    pressed: [bool; 2],
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("输入消息…")
                .auto_grow(3, 9)
                .submit_on_enter(true)
        });
        let changed = cx.subscribe_in(&input, window, |this: &mut Self, _, event, window, cx| {
            if matches!(event, KitEvent::Change | KitEvent::Focus | KitEvent::Blur) {
                if matches!(event, KitEvent::Blur) {
                    this.pressed = [false; 2];
                }
                let state = this.snapshot(window, cx);
                cx.emit(ComposerEvent::Changed(state));
            }
        });
        let weak = cx.entity().downgrade();
        let window_id = window.window_handle().window_id();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() != window_id {
                return;
            }
            let key = match event.keystroke.key.as_str() {
                "enter" => Key::Enter,
                "escape" => Key::Escape,
                _ => return,
            };
            let _ = weak.update(cx, |this, cx| {
                if !this.input.read(cx).focus_handle(cx).is_focused(window) {
                    return;
                }
                // 必须在 Kit 的 Enter/Escape 动作之前取 marked range。
                // 消费此键，避免 Kit 插入第二个换行或向会话再传播一次 Esc。
                cx.stop_propagation();
                let m = event.keystroke.modifiers;
                let held = std::mem::replace(&mut this.pressed[key_index(key)], true);
                this.dispatch(
                    InputEvent::KeyDown {
                        key,
                        modifiers: Modifiers {
                            shift: m.shift,
                            alt: m.alt,
                            control: m.control,
                            platform: m.platform,
                        },
                        held,
                    },
                    window,
                    cx,
                );
            });
        });
        Self {
            input,
            theme,
            send_enabled: false,
            pressed: [false; 2],
            _subscriptions: vec![changed, keys],
        }
    }

    /// 原生编辑接口供平台输入、附件粘贴和选择区操作使用；发送必须走 submit。
    pub fn editor(&self) -> &Entity<TextareaState> {
        &self.input
    }

    /// 每次决策直接读取真实编辑缓冲；Change 不是完整的 IME 生命周期通知。
    pub fn snapshot(&self, window: &mut Window, cx: &mut App) -> ComposerState {
        self.input.update(cx, |input, cx| ComposerState {
            text: input.value().to_string(),
            composing: input.marked_text_range(window, cx).is_some(),
            focused: input.focus_handle(cx).is_focused(window),
            send_enabled: self.send_enabled,
        })
    }

    pub fn set_send_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.send_enabled = enabled;
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// 按钮、命令面板和其他发送入口必须共用此组词守卫。
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(InputEvent::Submit, window, cx);
    }

    fn dispatch(&mut self, event: InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (_, actions) = step(self.snapshot(window, cx), event);
        for action in actions {
            match action {
                ComposerAction::InsertNewline => self.input.update(cx, |input, cx| {
                    input.replace_text_in_range(None, "\n", window, cx);
                }),
                ComposerAction::CancelComposition => self.input.update(cx, |input, cx| {
                    if let Some(marked) = input.marked_text_range(window, cx) {
                        input.replace_text_in_range(Some(marked), "", window, cx);
                        input.unmark_text(window, cx);
                    }
                }),
                action => cx.emit(ComposerEvent::Action(action)),
            }
        }
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = &self.theme;
        div()
            .flex()
            .flex_col()
            .gap(px(t.spacing.small))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                match event.keystroke.key.as_str() {
                    "enter" => this.pressed[key_index(Key::Enter)] = false,
                    "escape" => this.pressed[key_index(Key::Escape)] = false,
                    _ => {}
                }
            }))
            .p(px(t.spacing.medium))
            .bg(rgba(t.colors.surface))
            .border(px(t.border_width))
            .border_color(rgba(t.colors.border))
            .rounded(px(t.radius))
            .font_family(t.typography.family.clone())
            .text_size(px(t.typography.body))
            .text_color(rgba(t.colors.foreground))
            .child(
                Textarea::new(&self.input)
                    .appearance(false)
                    .bordered(false)
                    .p(px(t.spacing.small))
                    .text_size(px(t.typography.body))
                    .aria_label("消息输入框"),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(t.spacing.medium))
                    .text_size(px(t.typography.small))
                    .text_color(rgba(t.colors.muted))
                    .child("Enter 发送 · Shift/Alt+Enter 换行 · Esc 取消组词")
                    .child(
                        div()
                            .id("composer-submit")
                            .debug_selector(|| "composer-submit".into())
                            .cursor_pointer()
                            .text_color(rgba(if self.send_enabled {
                                t.colors.accent
                            } else {
                                t.colors.muted
                            }))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                            .child(if self.send_enabled {
                                "发送"
                            } else {
                                "暂不可发送"
                            }),
                    ),
            )
    }
}

fn key_index(key: Key) -> usize {
    match key {
        Key::Enter => 0,
        Key::Escape => 1,
    }
}
