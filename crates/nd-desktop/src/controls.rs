use crate::Desktop;
use gpui_kit::{prelude::*, *};
use nd_view_model::{Escape, accepted};
use nd_wire::{Command, CommandReply, Receipt};
use serde_json::{Value, json};

impl Desktop {
    fn interaction(&self) -> Value {
        self.session_snapshot
            .as_ref()
            .and_then(|s| s.items.iter().find(|i| i.kind == "header"))
            .map(|i| i.data["interaction"].clone())
            .unwrap_or(Value::Null)
    }
    pub(crate) fn escape_pressed(&mut self, cx: &mut Context<Self>) {
        let caps = self.interaction();
        let running = self.session_snapshot.as_ref().is_some_and(|s| {
            s.items
                .iter()
                .any(|i| i.kind == "header" && i.data["process"]["turn_running"] == true)
        });
        match self.escape.press(
            self.started.elapsed().as_millis() as u64,
            self.state.active_panel.is_some(),
            running,
            caps["rewind_menu"] == true,
        ) {
            Escape::ClosePanel => {
                self.state.active_panel = None;
            }
            Escape::RewindMenu => {
                self.state.active_panel = Some("rewind".into());
            }
            Escape::Interrupt if caps["interrupt"] == true => {
                self.control(
                    "session.interrupt",
                    json!({"session":self.state.selected_session}),
                    cx,
                );
            }
            Escape::Interrupt => self.warning = Some("当前进程不支持停止回合".into()),
            Escape::None => {}
        }
        self.save.send_replace(self.state.clone());
        cx.notify();
    }
    fn control(&mut self, name: &str, args: Value, cx: &mut Context<Self>) {
        let command = Command {
            id: uuid::Uuid::new_v4().to_string(),
            device: self.device.clone(),
            expect: json!({}),
            name: name.into(),
            args,
        };
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let result = client.command(command).await;
            let _ = weak.update(cx, |this, cx| {
                this.warning = match result {
                    Ok(reply) if accepted(&reply) => None,
                    Ok(CommandReply::Receipt {
                        receipt: Receipt::Rejected { code, .. },
                    }) => Some(format!("未受理：{code}；正文已保留")),
                    Ok(_) => Some("控制请求交付不明，请核对会话状态".into()),
                    Err(e) => Some(format!("控制请求未确认：{e}")),
                };
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn withdraw_message(
        &mut self,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
        if current.composing {
            return;
        }
        let key = self.draft_key();
        let draft = self.drafts.entry(key.clone()).or_default();
        draft.edit(current.text);
        let version = draft.version();
        let text = draft.text().to_owned();
        let attachments = draft.attachments().to_vec();
        self.control(
            "session.withdraw",
            json!({"session":key,"message":message,"draft":{"version":version,"text":text,"attachments":attachments}}),
            cx,
        );
    }
    fn cancel_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
        if current.composing {
            return;
        }
        let key = self.draft_key();
        let draft = self.drafts.entry(key.clone()).or_default();
        draft.edit(current.text);
        let version = draft.version();
        let text = draft.text().to_owned();
        let attachments = draft.attachments().to_vec();
        self.control(
            "session.interrupt",
            json!({"session":key,"queued":"cancel","draft":{"version":version,"text":text,"attachments":attachments}}),
            cx,
        );
    }

    pub(crate) fn chat_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let caps = self.interaction();
        let t = &self.theme;
        let intents = caps["send_intents"].as_array().cloned().unwrap_or_default();
        let mut row = div().flex().flex_col().gap(px(t.spacing.small));
        if !self.creating {
            row = row.child(div().flex().gap(px(t.spacing.medium)).children(
                intents.into_iter().filter_map(|intent| {
                    let name = intent.as_str()?.to_owned();
                    let label = match name.as_str() {
                        "fold" => "并入",
                        "after_turn" => "本回合后",
                        "interrupting" => "打断再发",
                        _ => return None,
                    };
                    let chosen = name == self.send_intent;
                    Some(
                        div()
                            .id(SharedString::from(format!("intent/{name}")))
                            .cursor_pointer()
                            .text_color(rgba(if chosen {
                                t.colors.accent
                            } else {
                                t.colors.muted
                            }))
                            .child(format!("{} {label}", if chosen { "●" } else { "○" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_intent = name.clone();
                                cx.notify();
                            })),
                    )
                }),
            ));
            row = row.child(
                div()
                    .text_color(rgba(t.colors.muted))
                    .text_size(px(t.typography.small))
                    .child(if caps["interrupt_spares_background"] == true {
                        "Esc 停止回合，保留后台任务 · 空闲双 Esc 打开回退菜单"
                    } else {
                        "Esc 会停止回合和后台任务"
                    }),
            );
            if caps["cancel_queued"] == true {
                row = row.child(
                    div()
                        .id("stop-and-cancel-queue")
                        .cursor_pointer()
                        .text_color(rgba(t.colors.accent))
                        .child("停止并撤回排队")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_queue(window, cx))),
                );
            }
            if self.send_intent == "interrupting" {
                row = row.child(div().text_color(rgba(t.colors.muted)).child(
                    if caps["immediate_preserves_mcp"] == true {
                        "可后台化的前台 MCP 将转后台；其余前台调用会中断"
                    } else {
                        "打断再发会中断前台 MCP 调用"
                    },
                ));
            }
        }
        if self.state.active_panel.as_deref() == Some("rewind") {
            let rounds = self
                .session_snapshot
                .as_ref()
                .and_then(|s| s.items.iter().find(|i| i.kind == "lineage"))
                .and_then(|i| i.data["rounds"].as_array())
                .cloned()
                .unwrap_or_default();
            row = row.child(
                div()
                    .p(px(t.spacing.medium))
                    .bg(rgba(t.colors.surface))
                    .rounded(px(t.radius))
                    .child("回退位置 · Esc 收起")
                    .children(
                        rounds
                            .iter()
                            .filter(|r| r["complete"] == true)
                            .map(|r| div().child(format!("第 {} 轮", r["n"]))),
                    )
                    .child(
                        div()
                            .text_color(rgba(t.colors.muted))
                            .child("此版本尚不可执行回退"),
                    ),
            );
        }
        row.into_any_element()
    }
}

#[cfg(feature = "scenarios")]
impl Desktop {
    /// 仅场景构建：驱动真实编辑器与同一产品动作，观测文本；不注入领域事实。
    pub fn scenario_controls(
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |weak, cx| {
            let mut last_action = String::new();
            let mut last_observation = Value::Null;
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                let command = std::fs::read(&path).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok());
                if weak.update_in(cx,|this,window,cx| {
                    if let Some(command) = &command
                        && let Some(id) = command["id"].as_str()
                        && id != last_action {
                        last_action = id.into();
                        match command["action"].as_str().unwrap() {
                            "edit" => { let editor = this.composer.read(cx).editor().clone(); editor.update(cx,|input,cx| { input.set_value(command["text"].as_str().unwrap(),window,cx); input.focus(window,cx); }); }
                            "intent" => this.send_intent = command["intent"].as_str().unwrap().into(),
                            "send" => { let composer = this.composer.clone(); composer.update(cx,|c,cx|c.submit(window,cx)); }
                            "withdraw" => {
                                let id = this.session_snapshot.as_ref().unwrap().items.iter().find(|i|i.kind == "prompt" && i.data["text"] == command["text"]).unwrap().data["message"].as_str().unwrap().to_owned();
                                this.withdraw_message(id,window,cx);
                            }
                            "escape" => { let composer = this.composer.clone(); composer.update(cx,|c,cx|c.scenario_escape(window,cx)); }
                            "panel" => this.state.active_panel = Some("settings".into()),
                            _ => panic!("unknown native scenario action"),
                        }
                        cx.notify();
                    }
                    let state = this.composer.update(cx,|c,cx|c.snapshot(window,cx));
                    let observation = json!({"action":last_action,"text":state.text,"panel":this.state.active_panel,"intent":this.send_intent,"warning":this.warning});
                    if observation != last_observation { println!("{}",json!({"native_controls":observation})); last_observation = observation; }
                }).is_err() { break; }
            }
        }).detach();
    }
}
