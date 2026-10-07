use crate::Desktop;
use gpui_kit::*;
use nd_view_model::{ThemeCatalog, ThemeMode, ThemeSelection};
use notify::Watcher;
use std::path::PathBuf;

/// 后台目录 I/O；空闲时只等文件通知，窗口销毁时关闭线程和 watcher。
pub(crate) struct ThemeFeed {
    updates: tokio::sync::watch::Receiver<ThemeCatalog>,
    pub reload: tokio::sync::mpsc::Sender<Result<(), String>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}
impl ThemeFeed {
    pub fn start(directory: PathBuf, initial: ThemeCatalog) -> std::io::Result<Self> {
        let (send, updates) = tokio::sync::watch::channel(initial);
        let (reload, mut events) = tokio::sync::mpsc::channel(1);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let notices = reload.clone();
        std::thread::Builder::new()
            .name("nd-themes".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build();
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        send.send_replace(ThemeCatalog {
                            entries: vec![],
                            warning: Some(format!("主题加载失败：{error}")),
                        });
                        return;
                    }
                };
                runtime.block_on(async move {
                    let watched = directory.clone();
                    let watcher = (|| {
                        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
                        let mut watcher = notify::recommended_watcher(
                            move |event: notify::Result<notify::Event>| match event {
                                Ok(event)
                                    if !matches!(event.kind, notify::EventKind::Access(_))
                                        && event.paths.iter().any(|p| p.starts_with(&watched)) =>
                                {
                                    let _ = notices.try_send(Ok(()));
                                }
                                Err(error) => {
                                    let _ = notices.try_send(Err(error.to_string()));
                                }
                                _ => {}
                            },
                        )
                        .map_err(|e| e.to_string())?;
                        // 父目录递归 watch 同时覆盖编辑器 rename 和主题目录删除/重建。
                        watcher
                            .watch(
                                directory.parent().unwrap_or(&directory),
                                notify::RecursiveMode::Recursive,
                            )
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(watcher)
                    })();
                    let mut watch_error = watcher.as_ref().err().cloned();
                    let _watcher = watcher;
                    let mut stopped = std::pin::pin!(stopped);
                    loop {
                        let mut catalog = ThemeCatalog::read(&directory);
                        if let Some(error) = &watch_error {
                            catalog.warning =
                                Some(format!("主题自动重载不可用：{error}；请点击重新加载"));
                        }
                        send.send_if_modified(|old| {
                            if old == &catalog {
                                false
                            } else {
                                *old = catalog;
                                true
                            }
                        });
                        tokio::select! {
                            _ = &mut stopped => break,
                            event = events.recv() => {
                                match event {
                                    None => break,
                                    Some(Err(error)) => watch_error = Some(error),
                                    _ => {}
                                }
                                // 一次编辑常有多条通知；不在 render/输入回调上读文件。
                                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                                while let Ok(event) = events.try_recv() {
                                    if let Err(error) = event { watch_error = Some(error); }
                                }
                            }
                        }
                    }
                });
            })?;
        Ok(Self {
            updates,
            reload,
            stop: Some(stop),
        })
    }
    pub async fn recv(&mut self) -> Option<ThemeCatalog> {
        self.updates.changed().await.ok()?;
        Some(self.updates.borrow_and_update().clone())
    }
}
impl Drop for ThemeFeed {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

pub(crate) fn system_mode(window: &Window) -> ThemeMode {
    match window.appearance() {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        _ => ThemeMode::Light,
    }
}

impl Desktop {
    /// 每设备外观偏好；不触碰守护进程配置或会话事实。
    pub fn select_theme(&mut self, selection: ThemeSelection, cx: &mut Context<Self>) {
        self.state.theme_selection = Some(selection);
        self.state.active_panel = None;
        self.resolve_theme(cx);
        self.save.send_replace(self.state.clone());
    }

    pub(crate) fn theme_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = match self.state.theme_selection() {
            ThemeSelection::System => "跟随系统".into(),
            ThemeSelection::Light => "默认浅色".into(),
            ThemeSelection::Dark => "默认深色".into(),
            ThemeSelection::File(file) => self
                .theme_catalog
                .entries
                .iter()
                .find(|e| e.file == file)
                .map(|e| e.name.clone())
                .unwrap_or(file),
        };
        control(
            div()
                .id("theme-menu")
                .relative()
                .cursor_pointer()
                .text_color(rgba(self.theme.colors.accent))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.active_panel =
                        if this.state.active_panel == Some(nd_view_model::Panel::Themes) {
                            None
                        } else {
                            Some(nd_view_model::Panel::Themes)
                        };
                    cx.notify();
                }))
                .child(format!("主题 · {name}")),
            "theme-menu".into(),
        )
        .into_any_element()
    }

    pub(crate) fn theme_panel(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.state.active_panel != Some(nd_view_model::Panel::Themes) {
            return None;
        }
        let mut choices = vec![
            (
                ThemeSelection::System,
                "跟随系统".to_owned(),
                "theme-system".to_owned(),
            ),
            (
                ThemeSelection::Light,
                "默认浅色".to_owned(),
                "theme-light".to_owned(),
            ),
            (
                ThemeSelection::Dark,
                "默认深色".to_owned(),
                "theme-dark".to_owned(),
            ),
        ];
        choices.extend(self.theme_catalog.entries.iter().map(|entry| {
            (
                ThemeSelection::File(entry.file.clone()),
                match &entry.theme {
                    Ok(_) => format!("{} · {}", entry.name, entry.file),
                    Err(error) => format!("{} · {error}", entry.file),
                },
                format!("theme-file-{}", entry.file),
            )
        }));
        let t = &self.theme;
        Some(
            div()
                .id("theme-panel")
                .flex_none()
                .max_h(window.viewport_size().height / 2.)
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p(px(t.spacing.medium))
                .gap(px(t.spacing.small))
                .bg(rgba(t.colors.surface))
                .border_b(px(t.border_width))
                .border_color(rgba(t.colors.border))
                .shadow(vec![box_shadow(t)])
                .child(
                    div()
                        .text_size(px(t.typography.small))
                        .text_color(rgba(t.colors.muted))
                        .child(format!(
                            "将主题 JSON 放入 {}，保存后自动生效",
                            self.theme_directory.display()
                        )),
                )
                .children(choices.into_iter().map(|(choice, name, id)| {
                    let selected = self.state.theme_selection() == choice;
                    control(
                        div()
                            .id(SharedString::from(id.clone()))
                            .relative()
                            .cursor_pointer()
                            .p(px(t.spacing.small))
                            .rounded(px(t.radius))
                            .text_color(rgba(if selected {
                                t.colors.accent
                            } else {
                                t.colors.foreground
                            }))
                            .child(format!("{} {name}", if selected { "✓" } else { "○" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_theme(choice.clone(), cx)
                            })),
                        id,
                    )
                }))
                .child(control(
                    div()
                        .id("theme-reload")
                        .relative()
                        .cursor_pointer()
                        .text_color(rgba(t.colors.accent))
                        .child("重新加载")
                        .on_click(cx.listener(|this, _, _, _| {
                            let _ = this.theme_reload.try_send(Ok(()));
                        })),
                    "theme-reload".into(),
                ))
                .into_any_element(),
        )
    }
    pub(crate) fn resolve_theme(&mut self, cx: &mut Context<Self>) {
        let resolved = self
            .theme_catalog
            .resolve(&self.state.theme_selection(), self.system_theme);
        self.theme_warning = resolved.warning;
        if self.theme != resolved.theme {
            self.set_theme(resolved.theme, cx);
        }
        cx.notify();
    }
}

pub(crate) fn box_shadow(theme: &nd_view_model::Theme) -> BoxShadow {
    let shadow = &theme.shadow;
    BoxShadow {
        color: rgba(shadow.color).into(),
        offset: point(px(shadow.offset_x), px(shadow.offset_y)),
        blur_radius: px(shadow.blur),
        spread_radius: px(shadow.spread),
        inset: shadow.inset,
    }
}

/// 导航提示也是应用视图；保持打开时也读取当前变量，不冻结一份旧主题。
pub(crate) fn tooltip(text: String, desktop: WeakEntity<Desktop>, cx: &mut App) -> AnyView {
    cx.new(|_| ThemeTooltip { text, desktop }).into()
}

struct ThemeTooltip {
    text: String,
    desktop: WeakEntity<Desktop>,
}
impl Render for ThemeTooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Ok(t) = self
            .desktop
            .read_with(cx, |desktop, _| desktop.theme.clone())
        else {
            return div().into_any_element();
        };
        div()
            .max_w(window.viewport_size().width / 2.)
            .m(px(t.spacing.small))
            .p(px(t.spacing.small))
            .bg(rgba(t.colors.surface))
            .text_color(rgba(t.colors.foreground))
            .font_family(t.typography.family.clone())
            .text_size(px(t.typography.small))
            .border(px(t.border_width))
            .border_color(rgba(t.colors.border))
            .rounded(px(t.radius))
            .shadow(vec![box_shadow(&t)])
            .child(self.text.clone())
            .into_any_element()
    }
}

fn control(element: Stateful<Div>, _id: String) -> Stateful<Div> {
    #[cfg(feature = "scenarios")]
    let element = element.child(
        canvas(
            |_, _, _| (),
            move |bounds, (), _, _| {
                println!(
                    "{}",
                    serde_json::json!({"theme_control":{"id":_id,"center":[
            f32::from(bounds.center().x), f32::from(bounds.center().y)]}})
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    );
    element
}

impl Desktop {
    /// 测试构建才有的原生输入驱动；点击走真实命中检测和产品 listener。
    #[cfg(feature = "scenarios")]
    pub fn scenario_theme_controls(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |weak, cx| {
            let mut last = None;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let path = path.clone();
                let next = cx
                    .background_executor()
                    .spawn(async move {
                        std::fs::read(path).ok().and_then(|bytes| {
                            serde_json::from_slice::<serde_json::Value>(&bytes).ok()
                        })
                    })
                    .await;
                if next == last {
                    continue;
                }
                last = next.clone();
                let Some(next) = next else { continue };
                if weak
                    .update_in(cx, |_, window, cx| {
                        if let Some(position) = next.get("click") {
                            let position = point(
                                px(position[0].as_f64().unwrap() as f32),
                                px(position[1].as_f64().unwrap() as f32),
                            );
                            window.defer(cx, move |window, cx| {
                                window.dispatch_event(
                                    PlatformInput::MouseMove(MouseMoveEvent {
                                        position,
                                        ..Default::default()
                                    }),
                                    cx,
                                );
                                window.dispatch_event(
                                    PlatformInput::MouseDown(MouseDownEvent {
                                        button: MouseButton::Left,
                                        position,
                                        modifiers: Modifiers::default(),
                                        click_count: 1,
                                        first_mouse: false,
                                    }),
                                    cx,
                                );
                                window.dispatch_event(
                                    PlatformInput::MouseUp(MouseUpEvent {
                                        button: MouseButton::Left,
                                        position,
                                        modifiers: Modifiers::default(),
                                        click_count: 1,
                                    }),
                                    cx,
                                );
                            });
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}
