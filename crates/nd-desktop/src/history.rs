use crate::Desktop;
use gpui_kit::{component::tooltip::Tooltip, prelude::*, *};
use nd_wire::PageReq;

impl Desktop {
    pub(crate) fn load_history(&mut self, page: PageReq, cx: &mut Context<Self>) {
        let Some(session) = self.state.selected_session.clone() else {
            return;
        };
        let generation = self.history.request();
        let client = self.history_client.clone();
        cx.spawn(async move |weak, cx| {
            let result = client.get(format!("session/{session}/items"), page).await;
            let _ = weak.update(cx, |this, cx| {
                if this.state.selected_session.as_ref() == Some(&session)
                    && this.history.loaded(generation, result)
                {
                    if this.history.error.is_none() {
                        this.scroll.set_offset(point(px(0.), px(0.)));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.creating || self.session_snapshot.is_none() {
            return div().into_any_element();
        }
        let width = self.theme.spacing.large * 2.;
        div()
            .w(px(width))
            .flex_none()
            .min_h_0()
            .child(
                uniform_list(
                    "round-navigation",
                    self.history.rounds().len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let theme = &this.theme;
                        range
                            .map(|index| {
                                let round = &this.history.rounds()[index];
                                let id = round.id.clone();
                                let preview = format!(
                                    "第 {} 轮\n{}",
                                    round.n,
                                    if round.preview.is_empty() {
                                        "历史尚不可用"
                                    } else {
                                        &round.preview
                                    }
                                );
                                let available = round.anchor.is_some();
                                let selected = this.history.anchor() == round.anchor.as_deref()
                                    && round.anchor.is_some();
                                #[cfg(feature = "scenarios")]
                                let bounds = this.navigation_bounds.clone();
                                #[cfg(feature = "scenarios")]
                                let bound_id = id.clone();
                                let mark = div()
                                    .relative()
                                    .id(SharedString::from(id.clone()))
                                    .h(px(theme.spacing.large))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .tooltip(move |window, cx| {
                                        #[cfg(feature = "scenarios")]
                                        println!(
                                            "{}",
                                            serde_json::json!({"history_preview":preview})
                                        );
                                        Tooltip::new(preview.clone()).build(window, cx)
                                    })
                                    .child(
                                        div()
                                            .w(px(theme.spacing.medium))
                                            .h(px(theme.border_width * 2.))
                                            .bg(rgba(if selected {
                                                theme.colors.accent
                                            } else {
                                                theme.colors.muted
                                            })),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if available {
                                            this.load_history(
                                                PageReq {
                                                    around: Some(id.clone()),
                                                    ..Default::default()
                                                },
                                                cx,
                                            );
                                        } else {
                                            this.history.error =
                                                Some("这一轮的历史尚不可用".into());
                                            cx.notify();
                                        }
                                    }));
                                #[cfg(feature = "scenarios")]
                                let mark = mark.child(
                                    canvas(
                                        move |b, _, _| {
                                            bounds.borrow_mut().insert(bound_id, b);
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                );
                                mark
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.navigation_scroll)
                .h_full(),
            )
            .into_any_element()
    }
    pub(crate) fn history_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.creating || self.session_snapshot.is_none() {
            return div().into_any_element();
        }
        let t = &self.theme;
        let mut buttons = Vec::new();
        for (label, before, after) in [
            ("更早", self.history.older(), None),
            ("较新", None, self.history.newer()),
        ] {
            if before.is_some() || after.is_some() {
                buttons.push(
                    div()
                        .id(label)
                        .cursor_pointer()
                        .text_color(rgba(t.colors.accent))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.load_history(
                                PageReq {
                                    before: before.clone(),
                                    after: after.clone(),
                                    ..Default::default()
                                },
                                cx,
                            );
                        }))
                        .into_any_element(),
                );
            }
        }
        if !self.history.is_latest() {
            buttons.push(
                div()
                    .id("latest-history")
                    .cursor_pointer()
                    .text_color(rgba(t.colors.accent))
                    .child("回到最新")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.history.latest();
                        this.scroll.scroll_to_bottom();
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .gap(px(t.spacing.medium))
            .px(px(t.spacing.large))
            .py(px(t.spacing.small))
            .children(buttons)
            .child(if self.history.loading {
                "正在读取历史…".to_owned()
            } else {
                self.history
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("{} 轮", self.history.rounds().len()))
            })
            .into_any_element()
    }
    #[cfg(feature = "scenarios")]
    pub fn scenario_history(plan: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |weak, cx| {
            let id = plan["round"].as_str().unwrap().to_owned();
            for _ in 0..400 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                if weak
                    .update(cx, |this, cx| {
                        let Some(index) = this.history.rounds().iter().position(|r| r.id == id)
                        else {
                            return false;
                        };
                        this.navigation_scroll
                            .scroll_to_item(index, ScrollStrategy::Center);
                        cx.notify();
                        true
                    })
                    .unwrap()
                {
                    break;
                }
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let position = weak
                .update_in(cx, |this, window, cx| {
                    let bounds = *this
                        .navigation_bounds
                        .borrow()
                        .get(&id)
                        .expect("visible navigation mark");
                    let position = bounds.center();
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position,
                            ..Default::default()
                        }),
                        cx,
                    );
                    position
                })
                .unwrap();
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1000))
                .await;
            cx.update(|window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        position,
                        button: MouseButton::Left,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        position,
                        button: MouseButton::Left,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
            })
            .unwrap();
        })
        .detach();
    }
}
