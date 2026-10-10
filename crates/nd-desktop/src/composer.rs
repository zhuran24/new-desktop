//! 输入法与编辑交给固定版 Kit，输入意图只从 nd-composer 产生。
use crate::keyboard_repeat::{KeyboardRepeat, RepeatTiming};
use crate::scenario_view::ScenarioBounds;
use gpui_kit::component::input::{InputEvent as KitEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nd_composer::{ComposerAction, ComposerState, InputEvent, Key, Modifiers, step};
use nd_view_model::Theme;
use std::time::{Duration, Instant};

struct EscapeBurst {
    last: Instant,
    timing: RepeatTiming,
    repeating: bool,
    cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    Changed(ComposerState),
    Attach(Vec<nd_ui_core::AttachmentSource>),
    Action(ComposerAction),
    CompositionClickBlocked,
}

/// 一个输入框实体对应一个编辑缓冲；宿主保持 Entity 和订阅的生命周期。
/// 不连接后端；Submit/Escape 由会话能力经同步副本处理。
pub struct Composer {
    input: Entity<TextareaState>,
    theme: Theme,
    send_enabled: bool,
    has_attachments: bool,
    paste_native: bool,
    paste_pending: bool,
    paste_generation: u64,
    pressed: [bool; 2],
    painted_composing: bool,
    pointer_composing: bool,
    escape_released_at: Option<Instant>,
    keyboard_repeat: KeyboardRepeat,
    uncommitted: Option<String>,
    escape_burst: Option<EscapeBurst>,
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
                    this.escape_released_at = None;
                    this.uncommitted = None;
                    this.escape_burst = None;
                }
                let state = this.snapshot(window, cx);
                // 标记变化会重画 Composer；平台可能在同一输入批次内先提交拼音，
                // 再分发鼠标按下，决策不能只看此时已被清掉的 marked range。
                if state.composing != this.painted_composing {
                    cx.notify();
                }
                cx.emit(ComposerEvent::Changed(state));
            }
        });
        let weak = cx.entity().downgrade();
        // Kit 的 preedit 更新只有 notify，没有 Change，必须观察编辑器本身。
        let preedit = cx.observe_in(&input, window, |this: &mut Self, _, window, cx| {
            this.observe_preedit(window, cx);
        });
        let window_id = window.window_handle().window_id();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() != window_id {
                return;
            }
            let _ = weak.update(cx, |this, cx| {
                if !this.input.read(cx).focus_handle(cx).is_focused(window) {
                    return;
                }
                let key = match event.keystroke.key.as_str() {
                    "enter" => {
                        this.escape_burst = None;
                        Key::Enter
                    }
                    "escape" => Key::Escape,
                    _ => {
                        this.escape_burst = None;
                        return;
                    }
                };
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
            has_attachments: false,
            paste_native: false,
            paste_pending: false,
            paste_generation: 0,
            pressed: [false; 2],
            painted_composing: false,
            pointer_composing: false,
            escape_released_at: None,
            keyboard_repeat: KeyboardRepeat::read(),
            uncommitted: None,
            escape_burst: None,
            _subscriptions: vec![changed, preedit, keys],
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
            has_attachments: self.has_attachments,
            composing: input.marked_text_range(window, cx).is_some(),
            focused: input.focus_handle(cx).is_focused(window),
            send_enabled: self.send_enabled,
        })
    }

    /// 宿主换草稿时撤销尚未完成的剪贴板读取，即使两份草稿的文字相同。
    pub fn cancel_pending_paste(&mut self) {
        self.paste_generation += 1;
        self.paste_pending = false;
    }
    pub fn set_has_attachments(&mut self, present: bool, cx: &mut Context<Self>) {
        self.has_attachments = present;
        cx.notify();
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

    /// 锁存这次按下之前画面中的组词状态，松开后的重画不能放行同一次点击。
    pub fn begin_pointer_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pointer_composing = self.painted_composing || self.snapshot(window, cx).composing;
        window.prevent_default();
        cx.stop_propagation();
    }

    /// 发送与导航共用同一个鼠标手势守卫；键盘和程序命令仍检查现场状态。
    pub fn finish_pointer_click(&mut self, cx: &mut Context<Self>) -> bool {
        let allowed = !std::mem::take(&mut self.pointer_composing);
        if !allowed {
            cx.emit(ComposerEvent::CompositionClickBlocked);
        }
        allowed
    }

    #[cfg(feature = "scenarios")]
    pub fn scenario_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
        self.dispatch(
            InputEvent::KeyDown {
                key: Key::Escape,
                modifiers: Modifiers::default(),
                held: false,
            },
            window,
            cx,
        );
    }

    /// 固定 GPUI 的 Wayland clipboard 只解文本/图片；文件 MIME 在这里补上。
    /// 查询在后台限时执行，普通文本/图片仍由 Kit 的原生 Paste 处理。
    fn paste(
        &mut self,
        _: &gpui_kit::component::input::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if std::mem::take(&mut self.paste_native) || std::env::var_os("WAYLAND_DISPLAY").is_none() {
            cx.propagate();
            return;
        }
        let state = self.snapshot(window, cx);
        if state.composing {
            cx.propagate();
            return;
        }
        cx.stop_propagation();
        if self.paste_pending {
            return;
        }
        self.paste_pending = true;
        let selection = self.input.read(cx).selected_range();
        let generation = self.paste_generation;
        cx.spawn_in(window, async move |weak, cx| {
            let paths = cx
                .background_executor()
                .spawn(async { wayland_files() })
                .await;
            let _ = weak.update_in(cx, |this, window, cx| {
                if generation != this.paste_generation {
                    return;
                }
                this.paste_pending = false;
                let current = this.snapshot(window, cx);
                if current.composing
                    || !current.focused
                    || current.text != state.text
                    || this.input.read(cx).selected_range() != selection
                {
                    return;
                }
                if let Some(paths) = paths {
                    cx.emit(ComposerEvent::Attach(
                        paths
                            .into_iter()
                            .map(nd_ui_core::AttachmentSource::Path)
                            .collect(),
                    ));
                } else {
                    this.paste_native = true;
                    window.dispatch_action(Box::new(gpui_kit::component::input::Paste), cx);
                }
            });
        })
        .detach();
    }

    fn dispatch(&mut self, event: InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.observe_preedit(window, cx);
        if matches!(
            event,
            InputEvent::KeyDown {
                key: Key::Escape,
                ..
            }
        ) && let Some(burst) = &mut self.escape_burst
        {
            // 首次转发在 repeat delay 后；后续间隔是 1/rate。
            // 允许一个周期的调度迟到/丢帧；不是全局 Esc 防抖。
            let gap = burst.timing.interval * 2;
            let window = if burst.repeating {
                gap
            } else {
                burst.timing.delay + gap
            };
            // 已分派过首个 Esc 时，延迟开始前的再按仍算新的一次。
            let earliest = if burst.cancelled || burst.repeating {
                Duration::ZERO
            } else {
                burst.timing.delay.saturating_sub(gap)
            };
            let elapsed = burst.last.elapsed();
            // Fcitx 的合成松开/按下紧邻；正常双按中间有真实的松开间隔。
            // 普通 Esc 还要求二者在半个重复周期内，避免短 repeat delay 吞掉双 Esc。
            let synthetic_release = self
                .escape_released_at
                .is_some_and(|up| up.elapsed() <= burst.timing.interval / 2);
            if (burst.cancelled || synthetic_release) && elapsed >= earliest && elapsed <= window {
                burst.last = Instant::now();
                burst.repeating = true;
                return;
            }
            self.escape_burst = None;
        }
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
                action => {
                    if action == ComposerAction::Escape {
                        self.escape_burst =
                            self.keyboard_repeat.timing().map(|timing| EscapeBurst {
                                last: Instant::now(),
                                timing,
                                repeating: false,
                                cancelled: false,
                            });
                    }
                    cx.emit(ComposerEvent::Action(action));
                }
            }
        }
    }

    fn observe_preedit(&mut self, window: &mut Window, cx: &mut App) {
        let (text, marked, focused) = self.input.update(cx, |input, cx| {
            (
                input.value().to_string(),
                input.marked_text_range(window, cx),
                input.focus_handle(cx).is_focused(window),
            )
        });
        if !focused {
            self.uncommitted = None;
            self.escape_burst = None;
        } else if let Some(marked) = marked {
            // marked range 是 UTF-16，编辑缓冲是 UTF-8；保留预编辑两侧的正文。
            let plain: Vec<_> = text.encode_utf16().collect();
            self.uncommitted =
                String::from_utf16(&plain[..marked.start])
                    .ok()
                    .and_then(|mut left| {
                        left.push_str(&String::from_utf16(&plain[marked.end..]).ok()?);
                        Some(left)
                    });
            self.escape_burst = None;
        } else if let Some(plain) = self.uncommitted.take()
            && plain == text
            && !self.pressed[key_index(Key::Escape)]
        {
            // 输入法消费了取消键：应用只见空 preedit，未收到首次 Esc。
            self.escape_burst = self.keyboard_repeat.timing().map(|timing| EscapeBurst {
                last: Instant::now(),
                timing,
                repeating: false,
                cancelled: true,
            });
        }
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.painted_composing = self.snapshot(window, cx).composing;
        #[cfg(feature = "scenarios")]
        let observer = {
            let weak = cx.entity().downgrade();
            Some(canvas(move |_, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    let state = this.snapshot(window, cx);
                    let bounds = this.input.read(cx).input_bounds();
                    println!("{}", serde_json::json!({"composer_view":{
                        "text":state.text,"composing":state.composing,"focused":state.focused,
                        "selection":this.input.read(cx).selected_range(),
                        "bounds":{"x":f32::from(bounds.origin.x),"y":f32::from(bounds.origin.y),
                            "width":f32::from(bounds.size.width),"height":f32::from(bounds.size.height)}
                    }}));
                });
            }, |_, (), _, _| {}).absolute().top_0().left_0().size_full().into_any_element())
        };
        #[cfg(not(feature = "scenarios"))]
        let observer = None::<AnyElement>;
        let t = &self.theme;
        div()
            .when(cfg!(feature = "scenarios"), |d| d.relative())
            .children(observer)
            .capture_action(cx.listener(Self::paste))
            .flex()
            .flex_col()
            .gap(px(t.spacing.small))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                match event.keystroke.key.as_str() {
                    "enter" => this.pressed[key_index(Key::Enter)] = false,
                    "escape" => {
                        this.pressed[key_index(Key::Escape)] = false;
                        this.escape_released_at = Some(Instant::now());
                    }
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
                    .aria_label("消息输入框")
                    .on_paste({
                        let weak = cx.entity().downgrade();
                        move |clipboard, _, cx| {
                            let sources: Vec<_> = clipboard
                                .entries
                                .iter()
                                .flat_map(|entry| match entry {
                                    ClipboardEntry::Image(image) => {
                                        vec![nd_ui_core::AttachmentSource::Bytes {
                                            name: format!(
                                                "粘贴图片.{}",
                                                image.format().extension()
                                            ),
                                            bytes: image.bytes().to_vec(),
                                        }]
                                    }
                                    ClipboardEntry::ExternalPaths(paths) => paths
                                        .0
                                        .iter()
                                        .cloned()
                                        .map(nd_ui_core::AttachmentSource::Path)
                                        .collect(),
                                    _ => vec![],
                                })
                                .collect();
                            if sources.is_empty() {
                                return false;
                            }
                            let _ =
                                weak.update(cx, |_, cx| cx.emit(ComposerEvent::Attach(sources)));
                            true
                        }
                    }),
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
                            .scenario_bounds("send")
                            .debug_selector(|| "composer-submit".into())
                            .cursor_pointer()
                            .text_color(rgba(if self.send_enabled {
                                t.colors.accent
                            } else {
                                t.colors.muted
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.begin_pointer_click(window, cx);
                                }),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.finish_pointer_click(cx) {
                                    this.submit(window, cx);
                                }
                            }))
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

fn wayland_files() -> Option<Vec<std::path::PathBuf>> {
    use std::{
        io::Read,
        process::{Command, Stdio},
    };
    // 不继承模型环境，也不启动 shell；只读当前桌面剪贴板。
    let mut command = Command::new("/usr/bin/timeout");
    command
        .args([
            "2",
            "/usr/bin/wl-paste",
            "--type",
            "text/uri-list",
            "--no-newline",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().ok()?;
    let mut bytes = vec![];
    let read = child
        .stdout
        .take()?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes);
    if bytes.len() > 64 * 1024 {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    if read.is_err() || !status.success() {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let paths: Vec<_> = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| url::Url::parse(line.trim()).ok()?.to_file_path().ok())
        .collect();
    (!paths.is_empty()).then_some(paths)
}
