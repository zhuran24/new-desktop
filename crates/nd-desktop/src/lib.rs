//! GPUI 适配层。能力视图经 Slots 登记，领域事实只来自 nd-wire。
mod attachments;
mod chat;
pub mod composer;
mod controls;
mod drafts;
mod history;
mod settings;
mod themes;
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nd_ui_core::{FeedUpdate, ReplicaFeed};
use nd_view_model::{Contribution, Slot, Slots, Theme, ThemeMode, ViewState};
use nd_wire::Snapshot;
use std::{path::PathBuf, rc::Rc};

pub type Renderer = Rc<dyn Fn(&Presentation<'_>, &mut Window, &mut App) -> AnyElement>;

/// 场景构建只读绘制后的控件几何；输入仍从私有合成器进入。
pub(crate) fn observed<E: Styled + ParentElement>(element: E, _id: &'static str) -> E {
    #[cfg(feature = "scenarios")]
    let element = element.relative().child(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                println!("{}", serde_json::json!({"native_layout": {
                    "id": _id, "x": f32::from(bounds.origin.x), "y": f32::from(bounds.origin.y),
                    "width": f32::from(bounds.size.width), "height": f32::from(bounds.size.height),
                    "viewport": [f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)]
                }}));
            },
        ).absolute().top_0().left_0().size_full(),
    );
    element
}
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
    history: nd_view_model::HistoryView,
    history_client: nd_ui_core::CommandClient,
    navigation_scroll: UniformListScrollHandle,
    #[cfg(feature = "scenarios")]
    navigation_bounds: Rc<std::cell::RefCell<std::collections::BTreeMap<String, Bounds<Pixels>>>>,
    session_feed: Option<Task<()>>,
    scroll: ScrollHandle,
    directory: Entity<InputState>,
    title_editor: Entity<InputState>,
    settings_open: bool,
    auxiliary_escape_held: bool,
    settings_sending: bool,
    model_picker: nd_view_model::ModelPicker,
    creating: bool,
    sending: bool,
    send_intent: String,
    escape: nd_view_model::EscapeState,
    started: std::time::Instant,
    uploading: usize,
    images: nd_view_model::ImageCache<std::sync::Arc<Image>>,
    device: String,
    draft_writes: std::collections::BTreeSet<String>,
    queued_send: Option<(String, String, u64, String)>,
    /// 已发出、输入框还没被守护进程清掉的 `!`/`/subtask`（会话，草稿修订号）：防止同一段文字连发两次。
    invoking: Option<(Option<String>, u64)>,
    /// 总结在途：这时不再接受另一次总结。
    compacting: bool,
    drafts: std::collections::BTreeMap<Option<String>, nd_view_model::Draft>,
    subscriptions: Vec<Subscription>,
    composer: Entity<composer::Composer>,
    state: ViewState,
    theme: Theme,
    theme_catalog: nd_view_model::ThemeCatalog,
    system_theme: ThemeMode,
    theme_warning: Option<String>,
    theme_directory: PathBuf,
    theme_reload: tokio::sync::mpsc::Sender<Result<(), String>>,
    _themes: Task<()>,
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
    #[cfg(feature = "scenarios")]
    last_theme_report: Option<serde_json::Value>,
    #[cfg(feature = "scenarios")]
    last_notice_report: Option<serde_json::Value>,
}
impl Desktop {
    pub fn new(
        socket: PathBuf,
        state: ViewState,
        save: tokio::sync::watch::Sender<ViewState>,
        warning: Option<String>,
        theme_directory: PathBuf,
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
        let system_theme = themes::system_mode(window);
        let _ = std::fs::create_dir_all(&theme_directory);
        let theme_catalog = nd_view_model::ThemeCatalog::read(&theme_directory);
        let resolved = theme_catalog.resolve(&state.theme_selection(), system_theme);
        let theme = resolved.theme;
        let mut theme_feed =
            themes::ThemeFeed::start(theme_directory.clone(), theme_catalog.clone())?;
        let theme_reload = theme_feed.reload.clone();
        let themes = cx.spawn(async move |weak, cx| {
            while let Some(catalog) = theme_feed.recv().await {
                if weak
                    .update(cx, |this: &mut Self, cx| {
                        this.theme_catalog = catalog;
                        this.resolve_theme(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        apply_theme(&theme, cx);
        let composer = cx.new(|cx| composer::Composer::new(theme.clone(), window, cx));
        let directory = cx.new(|cx| InputState::new(window, cx).placeholder("工作目录的绝对路径"));
        let mut this = Self {
            history_client: nd_ui_core::CommandClient::start(&socket)?,
            history: Default::default(),
            navigation_scroll: UniformListScrollHandle::new(),
            #[cfg(feature = "scenarios")]
            navigation_bounds: Default::default(),
            socket,
            client,
            session_snapshot: None,
            session_feed: None,
            scroll: ScrollHandle::new(),
            directory,
            title_editor: cx.new(|cx| InputState::new(window, cx).placeholder("会话标题")),
            settings_open: false,
            auxiliary_escape_held: false,
            settings_sending: false,
            model_picker: Default::default(),
            creating: state.selected_session.is_none(),
            sending: false,
            send_intent: "fold".into(),
            escape: Default::default(),
            started: std::time::Instant::now(),
            uploading: 0,
            images: Default::default(),
            device: format!("desktop-{}", uuid::Uuid::new_v4()),
            draft_writes: Default::default(),
            queued_send: None,
            invoking: None,
            compacting: false,
            drafts: Default::default(),
            subscriptions: vec![],
            composer,
            state,
            theme,
            theme_catalog,
            system_theme,
            theme_warning: resolved.warning,
            theme_directory,
            theme_reload,
            _themes: themes,
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
            #[cfg(feature = "scenarios")]
            last_theme_report: None,
            #[cfg(feature = "scenarios")]
            last_notice_report: None,
        };
        this.install_auxiliary_escape(window, cx);
        this.connect_chat(window, cx);
        this.subscriptions
            .push(cx.observe_window_appearance(window, |this, window, cx| {
                this.system_theme = themes::system_mode(window);
                this.resolve_theme(cx);
            }));
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
        kit.colors.link = rgba(theme.colors.accent).into();
        kit.colors.accent = rgba(theme.colors.background).into();
        kit.colors.accent_foreground = rgba(theme.colors.foreground).into();
        kit.colors.muted = rgba(theme.colors.background).into();
        kit.colors.table_head = rgba(theme.colors.background).into();
        kit.colors.table_head_foreground = rgba(theme.colors.foreground).into();
    });
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "scenarios")]
        {
            let report = serde_json::json!({"theme":self.theme,"warning":self.theme_warning,
                "system":self.system_theme,"selection":self.state.theme_selection(),"files":self.theme_catalog.entries.iter().map(|e| &e.file).collect::<Vec<_>>()});
            if self.last_theme_report.as_ref() != Some(&report) {
                println!("{}", serde_json::json!({"rendered_theme":report}));
                self.last_theme_report = Some(report);
            }
        }
        #[cfg(feature = "scenarios")]
        {
            let editor = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
            let report = serde_json::json!({"text":editor.text,"composing":editor.composing,"attachments": self.drafts.get(&self.draft_key()).map(|d| d.attachments()).unwrap_or_default(),
                "saved": self.drafts.get(&self.draft_key()).is_some_and(|d| d.is_saved() && d.text() == editor.text)});
            if self.last_editor_report.as_ref() != Some(&report) {
                println!("{}", serde_json::json!({"rendered_editor":report}));
                self.last_editor_report = Some(report);
            }
        }
        #[cfg(feature = "scenarios")]
        {
            // 会话头的降级提示与界面提示：真窗口场景据此核对界面算出的文字。
            let view = self
                .session_snapshot
                .as_ref()
                .map(nd_view_model::conversation);
            let report = serde_json::json!({
                "degraded": view.as_ref().and_then(|v| v.degraded.clone()),
                "abilities": view.as_ref().map(|v| serde_json::json!({"summarize":v.abilities.summarize,"shell":v.abilities.shell,"subtask":v.abilities.subtask})),
                "warning": self.warning,
            });
            if self.last_notice_report.as_ref() != Some(&report) {
                println!("{}", serde_json::json!({"rendered_notice":report}));
                self.last_notice_report = Some(report);
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
        #[cfg(feature = "scenarios")]
        if let Some(snapshot) = self.history.snapshot() {
            let view = nd_view_model::conversation(&snapshot);
            println!(
                "{}",
                serde_json::json!({"rendered_history":{
                    "rounds":self.history.rounds().len(),"anchor":self.history.anchor(),
                    "first":view.messages.first().map(|m|m.text.as_str()),"messages":view.messages.len()
                }})
            );
        }
        let header = self.render_slots(Slot::Header, window, cx);
        let sidebar = self.render_slots(Slot::Sidebar, window, cx);
        let mut right = self.render_slots(Slot::RightPanel, window, cx);
        match self.state.active_panel {
            Some(nd_view_model::Panel::Settings) => {
                right.extend(self.render_slots(Slot::Settings, window, cx))
            }
            Some(nd_view_model::Panel::Commands) => {
                right.extend(self.render_slots(Slot::CommandPalette, window, cx))
            }
            _ => {}
        }
        let chat_sidebar = self.chat_sidebar(cx);
        let chat_content = self.chat_content(window, cx);
        let chat_header = self.chat_header(cx);
        let controls = self.chat_controls(cx);
        let navigation = self.navigation(cx);
        let history_controls = self.history_controls(cx);
        let attachments = self.draft_attachments(cx);
        let theme_panel = self.theme_panel(window, cx);
        let draft_panel = (!self.creating).then(|| self.draft_panel(window, cx));
        let theme = &self.theme;
        div()
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                if event.keystroke.key == "escape" {
                    this.auxiliary_escape_held = false;
                }
            }))
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
                    .child(self.theme_button(cx)),
            )
            .children(theme_panel)
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
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .children(chat_header)
                            .child(history_controls)
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_h_0()
                                    .child(
                                        div()
                                            .id("content")
                                            .track_scroll(&self.scroll)
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .overflow_y_scroll()
                                            .flex()
                                            .flex_col()
                                            .p(px(theme.spacing.large))
                                            .gap(px(theme.spacing.medium))
                                            .child(chat_content),
                                    )
                                    .child(navigation)
                                    .when(true, |d| observed(d, "conversation")),
                            )
                            .child(
                                div()
                                    .id("attachment-drop")
                                    .flex()
                                    .flex_col()
                                    .flex_none()
                                    .max_h(window.viewport_size().height / 2.)
                                    .on_drop(cx.listener(
                                        |this, paths: &ExternalPaths, window, cx| {
                                            this.upload_attachments(
                                                paths
                                                    .0
                                                    .iter()
                                                    .cloned()
                                                    .map(nd_ui_core::AttachmentSource::Path)
                                                    .collect(),
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .p(px(theme.spacing.medium))
                                    .child(
                                        div()
                                            .id("draft-controls")
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scroll()
                                            .child(controls)
                                            .child(attachments)
                                            .children(draft_panel),
                                    )
                                    .child(div().flex_none().child(self.composer.clone()))
                                    .when(true, |d| observed(d, "composer")),
                            ),
                    )
                    .children(right),
            )
            .child(
                div()
                    .p(px(theme.spacing.small))
                    .text_size(px(theme.typography.small))
                    .text_color(rgba(theme.colors.muted))
                    .children(
                        self.theme_warning
                            .clone()
                            .map(|warning| div().child(warning)),
                    )
                    .child(self.warning.clone().unwrap_or_else(|| self.status.clone())),
            )
    }
}
