//! GPUI 适配层。能力视图经 Slots 登记，领域事实只来自 nd-wire。
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
        let mut feed = ReplicaFeed::start(socket, "global")?;
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
        let overview: Vec<Contribution<Renderer>> = vec![
            Contribution {
                slot: Slot::Sidebar,
                order: 0,
                value: Rc::new(|p, _, _| {
                    div()
                        .p(px(p.theme.spacing.small))
                        .text_color(rgba(p.theme.colors.muted))
                        .child("暂无会话")
                        .into_any_element()
                }),
            },
            Contribution {
                slot: Slot::Header,
                order: 0,
                value: Rc::new(|p, _, _| {
                    div()
                        .text_size(px(p.theme.typography.small))
                        .text_color(rgba(p.theme.colors.muted))
                        .child("本机")
                        .into_any_element()
                }),
            },
        ];
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
        Ok(Self {
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
        })
    }
    pub fn state(&self) -> &ViewState {
        &self.state
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
    });
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "scenarios")]
        if self.snapshot != self.last_report {
            if let Some(snapshot) = &self.snapshot {
                println!("{}", serde_json::json!({"rendered_snapshot": snapshot}));
            }
            self.last_report = self.snapshot.clone();
        }
        let theme = &self.theme;
        let header = self.render_slots(Slot::Header, window, cx);
        let sidebar = self.render_slots(Slot::Sidebar, window, cx);
        let mut right = self.render_slots(Slot::RightPanel, window, cx);
        match self.state.active_panel.as_deref() {
            Some("settings") => right.extend(self.render_slots(Slot::Settings, window, cx)),
            Some("commands") => right.extend(self.render_slots(Slot::CommandPalette, window, cx)),
            _ => {}
        }
        let mut items = Vec::new();
        if let Some(snapshot) = &self.snapshot {
            for item in nd_view_model::project(snapshot, &self.state).items {
                let renderers = self.slots.values(&Slot::Item(item.kind.clone()));
                if let Some(render) = renderers.first() {
                    items.push(render(
                        &Presentation {
                            snapshot: Some(snapshot),
                            state: &self.state,
                            theme,
                            item: Some(&item),
                        },
                        window,
                        cx,
                    ));
                } else {
                    items.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(theme.spacing.small))
                            .p(px(theme.spacing.medium))
                            .bg(rgba(theme.colors.surface))
                            .border(px(theme.border_width))
                            .border_color(rgba(theme.colors.border))
                            .rounded(px(theme.radius))
                            .shadow(vec![BoxShadow {
                                inset: theme.shadow.inset,
                                color: rgba(theme.shadow.color).into(),
                                offset: point(px(theme.shadow.offset_x), px(theme.shadow.offset_y)),
                                blur_radius: px(theme.shadow.blur),
                                spread_radius: px(theme.shadow.spread),
                            }])
                            .child(div().text_size(px(theme.typography.body)).child(item.title))
                            .child(div().text_color(rgba(theme.colors.muted)).child(item.text))
                            .into_any_element(),
                    );
                }
            }
        }
        div()
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
                            .child("会话")
                            .children(sidebar),
                    )
                    .child(
                        div()
                            .id("content")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .p(px(theme.spacing.large))
                            .gap(px(theme.spacing.medium))
                            .children(items),
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
