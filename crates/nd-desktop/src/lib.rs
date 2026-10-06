//! GPUI 适配层。能力视图经 Slots 登记，领域事实只来自 nd-wire。
mod chat;
pub mod composer;
mod controls;
mod drafts;
use gpui_kit::component::input::InputState;
use gpui_kit::*;
use nd_ui_core::{FeedUpdate, ReplicaFeed};
use nd_view_model::{Contribution, Slot, Slots, Theme, ThemeMode, ViewState};
use nd_wire::Snapshot;
use std::{path::PathBuf, rc::Rc};

pub type Renderer = Rc<dyn Fn(&Presentation<'_>, &mut Window, &mut App) -> AnyElement>;
pub struct Presentation<'a> {
    pub snapshot: Option<&'a Snapshot>,
    pub state: &'a ViewState,
    pub theme: &'a Theme,
    pub item: Option<&'a nd_view_model::ItemView>,
}

pub struct Desktop {
    socket: PathBuf,
    client: nd_ui_core::CommandClient,
    session_snapshot: Option<Snapshot>,
    session_feed: Option<Task<()>>,
    scroll: ScrollHandle,
    directory: Entity<InputState>,
    models: Vec<nd_wire::Model>,
    model: Option<String>,
    model_cwd: Option<String>,
    model_generation: u64,
    model_loading: bool,
    creating: bool,
    sending: bool,
    send_intent: String,
    escape: nd_view_model::EscapeState,
    started: std::time::Instant,
    device: String,
    draft_writes: std::collections::BTreeSet<String>,
    queued_send: Option<(String, String, u64, String)>,
    drafts: std::collections::BTreeMap<Option<String>, nd_view_model::Draft>,
    subscriptions: Vec<Subscription>,
    composer: Entity<composer::Composer>,
    state: ViewState,
    theme: Theme,
    snapshot: Option<Snapshot>,
    status: String,
    warning: Option<String>,
    pub slots: Slots<Renderer>,
    save: tokio::sync::watch::Sender<ViewState>,
    _feed: Task<()>,
    _slots_task: Task<()>,
    _bounds: Subscription,
    #[cfg(feature = "scenarios")]
    last_report: Option<Snapshot>,
    #[cfg(feature = "scenarios")]
    last_session_report: Option<Snapshot>,
    #[cfg(feature = "scenarios")]
    last_editor_report: Option<serde_json::Value>,
}
impl Desktop {
    pub fn new(
        socket: PathBuf,
        state: ViewState,
        save: tokio::sync::watch::Sender<ViewState>,
        warning: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> std::io::Result<Self> {
        let mut feed = ReplicaFeed::start(&socket, "global")?;
        let client = nd_ui_core::CommandClient::start(&socket)?;
        let task = cx.spawn(async move |weak, cx| {
            while let Some(update) = feed.recv().await {
                if weak
                    .update(cx, |this: &mut Self, cx| {
                        match update {
                            FeedUpdate::Snapshot(snapshot) => {
                                this.snapshot = Some(snapshot);
                                this.status = "已连接".into();
                            }
                            FeedUpdate::Unavailable(error) => {
                                this.snapshot = None;
                                this.status = format!("正在连接守护进程：{error}");
                            }
                        }
                        this.refresh_send(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            feed.close().await;
        });
        let bounds = cx.observe_window_bounds(window, |this: &mut Self, window, _cx| {
            let size = window.bounds().size;
            this.state.window.width = f32::from(size.width);
            this.state.window.height = f32::from(size.height);
            this.save.send_replace(this.state.clone());
        });
        let mut slots = Slots::<Renderer>::default();
        let overview: Vec<Contribution<Renderer>> = vec![Contribution {
            slot: Slot::Header,
            order: 0,
            value: Rc::new(|p, _, _| {
                div()
                    .text_size(px(p.theme.typography.small))
                    .text_color(rgba(p.theme.colors.muted))
                    .child("本机")
                    .into_any_element()
            }),
        }];
        slots
            .configure(
                "overview",
                &overview,
                state.components.get("overview").copied().unwrap_or(true),
            )
            .expect("built-in slots");
        let mut changed = slots.changed();
        let slots_task = cx.spawn(async move |weak, cx| {
            loop {
                changed.await;
                match weak.update(cx, |this: &mut Self, cx| {
                    cx.notify();
                    this.slots.changed()
                }) {
                    Ok(next) => changed = next,
                    Err(_) => break,
                }
            }
        });
        let theme = Theme::builtin(state.theme);
        apply_theme(&theme, cx);
        let composer = cx.new(|cx| composer::Composer::new(theme.clone(), window, cx));
        let directory = cx.new(|cx| InputState::new(window, cx).placeholder("工作目录的绝对路径"));
        let mut this = Self {
            socket,
            client,
            session_snapshot: None,
            session_feed: None,
            scroll: ScrollHandle::new(),
            directory,
            models: vec![],
            model: None,
            model_cwd: None,
            model_generation: 0,
            model_loading: false,
            creating: state.selected_session.is_none(),
            sending: false,
            send_intent: "fold".into(),
            escape: Default::default(),
            started: std::time::Instant::now(),
            device: format!("desktop-{}", uuid::Uuid::new_v4()),
            draft_writes: Default::default(),
            queued_send: None,
            drafts: Default::default(),
            subscriptions: vec![],
            composer,
            state,
            theme,
            snapshot: None,
            status: "正在连接守护进程…".into(),
            warning,
            slots,
            save,
            _feed: task,
            _slots_task: slots_task,
            _bounds: bounds,
            #[cfg(feature = "scenarios")]
            last_report: None,
            #[cfg(feature = "scenarios")]
            last_session_report: None,
            #[cfg(feature = "scenarios")]
            last_editor_report: None,
        };
        this.connect_chat(window, cx);
        Ok(this)
    }
    pub fn state(&self) -> &ViewState {
        &self.state
    }
    /// 产品输入框；其动作已经接入同步副本，不应再登记一个发送者。
    pub fn composer(&self) -> &Entity<composer::Composer> {
        &self.composer
    }
    pub fn theme(&self) -> &Theme {
        &self.theme
    }
    /// 视图偏好经此入口修改并异步保存；不改同步副本。
    pub fn update_view_state(
        &mut self,
        update: impl FnOnce(&mut ViewState),
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        let mut state = self.state.clone();
        update(&mut state);
        state.validate()?;
        self.state = state;
        self.save.send_replace(self.state.clone());
        cx.notify();
        Ok(())
    }
    pub fn configure_component(
        &mut self,
        name: &str,
        entries: &[Contribution<Renderer>],
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.slots.configure(name, entries, enabled)?;
        self.state.components.insert(name.into(), enabled);
        self.save.send_replace(self.state.clone());
        cx.notify();
        Ok(())
    }
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.state.theme = theme.mode;
        self.theme = theme;
        apply_theme(&self.theme, cx);
        self.composer.update(cx, |composer, cx| {
            composer.set_theme(self.theme.clone(), cx)
        });
        self.save.send_replace(self.state.clone());
        cx.notify();
    }
    fn render_slots(&self, slot: Slot, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        let presentation = Presentation {
            snapshot: self.snapshot.as_ref(),
            state: &self.state,
            theme: &self.theme,
            item: None,
        };
        self.slots
            .values(&slot)
            .iter()
            .map(|render| render(&presentation, window, cx))
            .collect()
    }
}

pub fn apply_theme(theme: &Theme, cx: &mut App) {
    use gpui_kit::component::{Theme as KitTheme, ThemeMode as KitMode};
    KitTheme::change(
        match theme.mode {
            ThemeMode::Light => KitMode::Light,
            ThemeMode::Dark => KitMode::Dark,
        },
        None,
        cx,
    );
    KitTheme::update(cx, |kit| {
        kit.font_family = theme.typography.family.clone().into();
        kit.font_size = px(theme.typography.body);
        kit.mono_font_family = theme.typography.mono_family.clone().into();
        kit.mono_font_size = px(theme.typography.body);
        kit.radius = px(theme.radius);
        kit.radius_lg = px(theme.radius);
        kit.colors.background = rgba(theme.colors.background).into();
        kit.colors.foreground = rgba(theme.colors.foreground).into();
        kit.colors.border = rgba(theme.colors.border).into();
        kit.colors.input = rgba(theme.colors.border).into();
        kit.colors.caret = rgba(theme.colors.accent).into();
        kit.colors.ring = rgba(theme.colors.accent).into();
        kit.colors.muted_foreground = rgba(theme.colors.muted).into();
        kit.colors.selection = rgba(theme.colors.accent).into();
    });
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "scenarios")]
        {
            let editor = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
            let report = serde_json::json!({"text":editor.text,"composing":editor.composing,
                "saved": self.drafts.get(&self.draft_key()).is_some_and(|d| d.is_saved() && d.text() == editor.text)});
            if self.last_editor_report.as_ref() != Some(&report) {
                println!("{}", serde_json::json!({"rendered_editor":report}));
                self.last_editor_report = Some(report);
            }
        }
        #[cfg(feature = "scenarios")]
        if self.snapshot != self.last_report {
            if let Some(snapshot) = &self.snapshot {
                println!("{}", serde_json::json!({"rendered_snapshot": snapshot}));
            }
            self.last_report = self.snapshot.clone();
        }
        #[cfg(feature = "scenarios")]
        if self.session_snapshot != self.last_session_report {
            if let Some(snapshot) = &self.session_snapshot {
                println!("{}", serde_json::json!({"rendered_session": snapshot}));
            }
            self.last_session_report = self.session_snapshot.clone();
        }
        let header = self.render_slots(Slot::Header, window, cx);
        let sidebar = self.render_slots(Slot::Sidebar, window, cx);
        let mut right = self.render_slots(Slot::RightPanel, window, cx);
        match self.state.active_panel.as_deref() {
            Some("settings") => right.extend(self.render_slots(Slot::Settings, window, cx)),
            Some("commands") => right.extend(self.render_slots(Slot::CommandPalette, window, cx)),
            _ => {}
        }
        let chat_sidebar = self.chat_sidebar(cx);
        let chat_content = self.chat_content(window, cx);
        let controls = self.chat_controls(cx);
        let draft_panel = (!self.creating).then(|| self.draft_panel(window, cx));
        let theme = &self.theme;
        div()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.is_held {
                    this.escape_pressed(cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgba(theme.colors.background))
            .text_color(rgba(theme.colors.foreground))
            .font_family(theme.typography.family.clone())
            .text_size(px(theme.typography.body))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p(px(theme.spacing.medium))
                    .gap(px(theme.spacing.medium))
                    .border_b(px(theme.border_width))
                    .border_color(rgba(theme.colors.border))
                    .child(
                        div()
                            .text_size(px(theme.typography.title))
                            .child("New Desktop"),
                    )
                    .children(header)
                    .child(
                        div()
                            .id("theme-toggle")
                            .cursor_pointer()
                            .text_color(rgba(theme.colors.accent))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let mode = match this.theme.mode {
                                    ThemeMode::Light => ThemeMode::Dark,
                                    ThemeMode::Dark => ThemeMode::Light,
                                };
                                this.set_theme(Theme::builtin(mode), cx);
                            }))
                            .child(match theme.mode {
                                ThemeMode::Light => "切换深色",
                                ThemeMode::Dark => "切换浅色",
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(self.state.sidebar_width))
                            .flex_none()
                            .p(px(theme.spacing.medium))
                            .border_r(px(theme.border_width))
                            .border_color(rgba(theme.colors.border))
                            .id("session-list")
                            .overflow_y_scroll()
                            .child("会话")
                            .child(chat_sidebar)
                            .children(sidebar),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("content")
                                    .track_scroll(&self.scroll)
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .p(px(theme.spacing.large))
                                    .gap(px(theme.spacing.medium))
                                    .child(chat_content),
                            )
                            .child(
                                div()
                                    .p(px(theme.spacing.medium))
                                    .child(controls)
                                    .children(draft_panel)
                                    .child(self.composer.clone()),
                            ),
                    )
                    .children(right),
            )
            .child(
                div()
                    .p(px(theme.spacing.small))
                    .text_size(px(theme.typography.small))
                    .text_color(rgba(theme.colors.muted))
                    .child(self.warning.clone().unwrap_or_else(|| self.status.clone())),
            )
    }
}
