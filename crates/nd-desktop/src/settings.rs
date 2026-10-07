use crate::Desktop;
use gpui_kit::{component::input::Input, prelude::*, *};
use nd_wire::{Command, CommandReply, LiveSetting, Receipt};
use serde_json::json;

impl Desktop {
    fn setting_command(&mut self, name: &str, mut args: serde_json::Value, cx: &mut Context<Self>) {
        if self.settings_sending {
            return;
        }
        let Some(snapshot) = &self.session_snapshot else {
            return;
        };
        let Some(header) = snapshot.items.iter().find(|i| i.kind == "header") else {
            return;
        };
        args["session"] = header.data["session"].clone();
        let field = if name == "session.rename" {
            "title_revision"
        } else {
            "settings_revision"
        };
        let command = Command {
            id: uuid::Uuid::new_v4().to_string(),
            device: self.device.clone(),
            name: name.into(),
            args,
            expect: json!({field:header.data[field].as_u64().unwrap_or(0)}),
        };
        let client = self.client.clone();
        self.settings_sending = true;
        cx.spawn(async move |weak, cx| {
            let result = client.command(command).await;
            let _ = weak.update(cx, |this, cx| {
                this.settings_sending = false;
                this.warning = match result {
                    Ok(CommandReply::Receipt {
                        receipt: Receipt::Accepted { .. } | Receipt::Done { .. },
                    }) => None,
                    Ok(CommandReply::Receipt {
                        receipt: Receipt::Rejected { code, .. },
                    }) => Some(match code.as_str() {
                        "conflict" => "设置已在别处改变，请查看当前值后重试".into(),
                        "busy" => "会话正在应用操作，请稍后重试".into(),
                        "invalid_title" => "标题需为 1–200 个字符，不能包含换行或控制字符".into(),
                        "unsupported" => "当前后端进程不提供这项设置".into(),
                        _ => format!("设置未受理：{code}"),
                    }),
                    Ok(_) => Some("设置尚未确认，请检查会话中的操作结果".into()),
                    Err(error) => Some(format!("设置尚未确认：{error}")),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn configure_session(&mut self, setting: LiveSetting, cx: &mut Context<Self>) {
        self.setting_command("session.configure", json!({"setting":setting}), cx);
    }
    fn rename_session(&mut self, cx: &mut Context<Self>) {
        self.setting_command(
            "session.rename",
            json!({"title":self.title_editor.read(cx).value().to_string()}),
            cx,
        );
    }
    pub(crate) fn open_session_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            let title = self
                .session_snapshot
                .as_ref()
                .map(nd_view_model::session_settings)
                .unwrap_or_default()
                .title;
            self.title_editor
                .update(cx, |input, cx| input.set_value(title, window, cx));
        }
        cx.notify();
    }
    pub(crate) fn session_settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.theme;
        let view = self
            .session_snapshot
            .as_ref()
            .map(nd_view_model::session_settings)
            .unwrap_or_default();
        let mut panel = div().flex().flex_col().gap(px(t.spacing.small)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(view.title.clone())
                .child(
                    div()
                        .id("session-settings-toggle")
                        .cursor_pointer()
                        .text_color(rgba(t.colors.accent))
                        .child(if self.settings_open {
                            "收起设置"
                        } else {
                            "会话设置"
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_session_settings(window, cx)
                        })),
                ),
        );
        if !self.settings_open {
            return panel.into_any_element();
        }
        let busy = self.settings_sending || view.pending.is_some();
        let option = |id: String, label: String, selected: bool, disabled: bool| {
            div()
                .id(SharedString::from(id))
                .px(px(t.spacing.medium))
                .py(px(t.spacing.small))
                .rounded(px(t.radius))
                .bg(rgba(if selected {
                    t.colors.accent
                } else {
                    t.colors.surface
                }))
                .text_color(rgba(if disabled {
                    t.colors.muted
                } else if selected {
                    t.colors.background
                } else {
                    t.colors.foreground
                }))
                .cursor_pointer()
                .child(label)
        };
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .gap(px(t.spacing.small))
                .child("模型 · 下一回合起生效")
                .child(div().flex().flex_wrap().gap(px(t.spacing.small)).children(
                    view.models.iter().map(|m| {
                        let setting = LiveSetting::Model(m.value.clone());
                        let disabled = busy || m.disabled;
                        option(
                            format!("session-model/{}", m.value),
                            m.label.clone(),
                            m.selected,
                            disabled,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !disabled {
                                this.configure_session(setting.clone(), cx);
                            }
                        }))
                    }),
                )),
        );
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(px(t.spacing.small))
                .child("Effort")
                .children(view.efforts.iter().map(|effort| {
                    let setting = LiveSetting::Effort(effort.clone());
                    option(
                        format!("session-effort/{effort}"),
                        effort.clone(),
                        *effort == view.effort,
                        busy,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !busy {
                            this.configure_session(setting.clone(), cx);
                        }
                    }))
                })),
        );
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(px(t.spacing.small))
                .child("权限模式")
                .children(view.permission_modes.iter().map(|mode| {
                    let label = match mode.as_str() {
                        "default" => "默认审批",
                        "acceptEdits" => "允许编辑",
                        "plan" => "计划",
                        "dontAsk" => "不询问",
                        "auto" => "自动",
                        "bypassPermissions" => "跳过审批",
                        _ => mode,
                    };
                    let setting = LiveSetting::PermissionMode(mode.clone());
                    option(
                        format!("session-mode/{mode}"),
                        label.into(),
                        *mode == view.permission_mode,
                        busy,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !busy {
                            this.configure_session(setting.clone(), cx);
                        }
                    }))
                })),
        );
        if let Some(on) = view.ultracode {
            let requested = view.ultracode_requested;
            panel = panel.child(
                option(
                    "session-ultracode".into(),
                    format!("ultracode：{}", if on { "已开启" } else { "已关闭" }),
                    on,
                    busy,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !busy {
                        this.configure_session(LiveSetting::Ultracode(!requested), cx);
                    }
                })),
            );
        }
        if let Some(pending) = view.pending {
            panel = panel.child(div().text_color(rgba(t.colors.muted)).child(pending));
        }
        panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(t.spacing.small))
                    .child(Input::new(&self.title_editor))
                    .child(
                        option(
                            "session-rename".into(),
                            "保存标题".into(),
                            false,
                            self.settings_sending,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.rename_session(cx))),
                    ),
            )
            .into_any_element()
    }

    #[cfg(feature = "scenarios")]
    pub fn scenario_settings(
        _plan: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |weak, cx| {
            let mut stage = 0;
            for _ in 0..350 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let done = weak
                    .update_in(cx, |this, window, cx| {
                        let Some(snapshot) = &this.session_snapshot else {
                            return false;
                        };
                        let Some(h) = snapshot
                            .items
                            .iter()
                            .find(|i| i.kind == "header")
                            .map(|i| i.data.clone())
                        else {
                            return false;
                        };
                        if h["status"] != "active" || !h["op"].is_null() || this.settings_sending {
                            return false;
                        }
                        match stage {
                            0 => {
                                this.open_session_settings(window, cx);
                                this.configure_session(LiveSetting::Model("opus".into()), cx);
                            }
                            1 if h["model"] == "opus" => {
                                this.configure_session(LiveSetting::Effort("high".into()), cx)
                            }
                            2 if h["settings"]["applied"]["effort"] == "high" => {
                                this.title_editor.update(cx, |input, cx| {
                                    input.set_value("原生设置标题", window, cx)
                                });
                                this.rename_session(cx);
                            }
                            3 if h["title"] == "原生设置标题" => return true,
                            _ => return false,
                        }
                        stage += 1;
                        false
                    })
                    .unwrap();
                if done {
                    return;
                }
            }
            panic!("native settings scenario timed out");
        })
        .detach();
    }
}
