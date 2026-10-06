use crate::{Desktop, Presentation, composer::ComposerEvent};
use gpui_kit::prelude::*;
use gpui_kit::{
    component::{
        input::{Input, InputEvent},
        text::{TextView, TextViewStyle},
    },
    *,
};
use nd_composer::ComposerAction;
use nd_ui_core::{FeedUpdate, ReplicaFeed};
use nd_view_model::{Slot, accepted, conversation, sidebar};
use nd_wire::{Command, CommandReply, Receipt};
use serde_json::json;

impl Desktop {
    /// 隔离真窗口场景：只驱动产品输入与动作，不注入副本或后端事实。
    #[cfg(feature = "scenarios")]
    pub fn scenario_create(plan: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |weak, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            weak.update_in(cx, |this, window, cx| {
                this.directory.update(cx, |input, cx| {
                    input.set_value(plan["cwd"].as_str().unwrap(), window, cx)
                });
            })
            .unwrap();
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            weak.update(cx, |this, cx| this.load_models(cx)).unwrap();
            for _ in 0..500 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let done = weak
                    .update_in(cx, |this, window, cx| {
                        if this.model_loading || this.snapshot.is_none() {
                            return false;
                        }
                        let value = plan["model"].as_str().unwrap();
                        assert!(
                            this.models.iter().any(|m| m.value == value && !m.disabled),
                            "model missing: {:?}",
                            this.warning
                        );
                        this.model = Some(value.into());
                        this.refresh_send(cx);
                        let composer = this.composer.clone();
                        composer.update(cx, |composer, cx| {
                            composer.editor().update(cx, |input, cx| {
                                input.set_value(plan["text"].as_str().unwrap(), window, cx);
                                input.focus(window, cx);
                            });
                            composer.submit(window, cx);
                        });
                        true
                    })
                    .unwrap();
                if done {
                    return;
                }
            }
            panic!("native scenario did not become ready");
        })
        .detach();
    }
    pub(crate) fn connect_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.subscriptions.push(cx.subscribe_in(
            &self.composer,
            window,
            |this, _, event, window, cx| match event {
                ComposerEvent::Changed(state) => {
                    if state.composing {
                        return;
                    }
                    let key = this.draft_key();
                    let draft = this.drafts.entry(key.clone()).or_default();
                    let revision = draft.revision();
                    draft.edit(state.text.clone());
                    if revision != draft.revision() {
                        this.queued_send = None;
                    }
                    if let Some(session) = key {
                        this.sync_draft(window, cx);
                        this.persist_draft(session, window, cx);
                    }
                    this.refresh_send(cx);
                    cx.notify();
                }
                ComposerEvent::Action(ComposerAction::Submit { text }) => {
                    this.send_text(text.clone(), window, cx)
                }
                ComposerEvent::Action(ComposerAction::Escape) => {
                    this.escape_pressed(cx);
                }
                _ => {}
            },
        ));
        self.subscriptions
            .push(cx.subscribe(&self.directory, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.model_generation += 1;
                    this.models.clear();
                    this.model = None;
                    this.model_cwd = None;
                    this.model_loading = false;
                    this.refresh_send(cx);
                    cx.notify();
                }
            }));
        if let Some(session) = self.state.selected_session.clone() {
            self.select_session(session, window, cx);
        }
    }
    pub(crate) fn draft_key(&self) -> Option<String> {
        if self.creating {
            None
        } else {
            self.state.selected_session.clone()
        }
    }
    pub(crate) fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .drafts
            .entry(self.draft_key())
            .or_default()
            .text()
            .to_owned();
        let input = self.composer.read(cx).editor().clone();
        let current = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
        if current.composing || current.text == text {
            return;
        }
        input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
    }
    pub(crate) fn refresh_send(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.sending
            && self.queued_send.is_none()
            && !self
                .drafts
                .get(&self.draft_key())
                .is_some_and(|d| d.needs_send_review())
            && self.snapshot.is_some()
            && if self.creating {
                self.model_cwd.as_deref() == Some(self.directory.read(cx).value().as_ref())
                    && self
                        .models
                        .iter()
                        .any(|m| Some(&m.value) == self.model.as_ref() && !m.disabled)
            } else {
                self.session_snapshot
                    .as_ref()
                    .is_some_and(|s| conversation(s).can_send)
            };
        self.composer
            .update(cx, |c, cx| c.set_send_enabled(enabled, cx));
    }
    pub fn begin_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .composer
            .update(cx, |c, cx| c.snapshot(window, cx))
            .composing
        {
            self.warning = Some("请先完成组词，再切换会话".into());
            cx.notify();
            return;
        }
        self.queued_send = None;
        self.scroll = ScrollHandle::new();
        self.escape = Default::default();
        self.send_intent = "fold".into();
        self.creating = true;
        self.session_feed = None;
        self.session_snapshot = None;
        self.state.selected_session = None;
        self.save.send_replace(self.state.clone());
        self.restore_draft(window, cx);
        self.refresh_send(cx);
        cx.notify();
    }
    pub fn select_session(&mut self, session: String, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .composer
            .update(cx, |c, cx| c.snapshot(window, cx))
            .composing
        {
            self.warning = Some("请先完成组词，再切换会话".into());
            cx.notify();
            return;
        }
        self.queued_send = None;
        self.scroll = ScrollHandle::new();
        self.escape = Default::default();
        self.send_intent = "fold".into();
        self.session_feed = None; // 丢掉接收任务即关闭订阅，不结束后端。
        self.session_snapshot = None;
        self.creating = false;
        self.state.selected_session = Some(session.clone());
        self.save.send_replace(self.state.clone());
        self.restore_draft(window, cx);
        self.refresh_send(cx);
        let stream = format!("session/{session}");
        match ReplicaFeed::start(&self.socket, &stream) {
            Ok(mut feed) => {
                self.session_feed = Some(cx.spawn_in(window, async move |weak, cx| {
                    while let Some(update) = feed.recv().await {
                        if weak
                            .update_in(cx, |this, window, cx| {
                                if this.state.selected_session.as_ref() != Some(&session)
                                    || this.creating
                                {
                                    return;
                                }
                                match update {
                                    FeedUpdate::Snapshot(s) => {
                                        if this.session_snapshot.is_none()
                                            || this.scroll.max_offset().y + this.scroll.offset().y
                                                <= px(this.theme.spacing.small)
                                        {
                                            this.scroll.scroll_to_bottom();
                                        }
                                        this.session_snapshot = Some(s);
                                        this.sync_draft(window, cx);
                                        this.persist_draft(session.clone(), window, cx);
                                    }
                                    FeedUpdate::Unavailable(e) => {
                                        this.session_snapshot = None;
                                        this.warning = Some(e);
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
                }));
            }
            Err(e) => self.warning = Some(e.to_string()),
        }
        cx.notify();
    }
    pub fn load_models(&mut self, cx: &mut Context<Self>) {
        let cwd = self.directory.read(cx).value().to_string();
        self.models.clear();
        self.model = None;
        self.model_cwd = None;
        self.model_generation += 1;
        let generation = self.model_generation;
        self.model_loading = true;
        self.warning = None;
        self.refresh_send(cx);
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let result = client.models("claude".into(), cwd.clone()).await;
            let _ = weak.update(cx, |this, cx| {
                if generation != this.model_generation {
                    return;
                }
                this.model_loading = false;
                match result {
                    Ok(models) => {
                        this.model = models.iter().find(|m| !m.disabled).map(|m| m.value.clone());
                        this.models = models;
                        this.model_cwd = Some(cwd);
                        if this.model.is_none() {
                            this.warning = Some("后端没有可用模型".into());
                        }
                    }
                    Err(e) => this.warning = Some(format!("获取模型失败：{e}")),
                }
                this.refresh_send(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn send_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.send_text_intent(text, self.send_intent.clone(), window, cx);
    }
    pub(crate) fn send_text_intent(
        &mut self,
        text: String,
        intent: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sending
            || !self
                .composer
                .update(cx, |c, cx| c.snapshot(window, cx))
                .send_enabled
        {
            return;
        }
        let key = self.draft_key();
        let draft = self.drafts.entry(key.clone()).or_default();
        draft.edit(text.clone());
        let revision = draft.revision();
        let version = draft.version();
        if let Some(session) = key.clone()
            && !draft.is_saved()
        {
            self.queued_send = Some((session.clone(), text, revision, intent));
            self.persist_draft(session, window, cx);
            self.refresh_send(cx);
            cx.notify();
            return;
        }
        let command = Command {
            id: uuid::Uuid::new_v4().to_string(),
            device: self.device.clone(),
            expect: json!({"draft_version":version}),
            name: if self.creating {
                "session.create"
            } else {
                "session.send"
            }
            .into(),
            args: if self.creating {
                json!({"cwd":self.directory.read(cx).value().to_string(), "model":self.model, "text":text})
            } else {
                json!({"session":key, "text":text, "intent":intent})
            },
        };
        self.sending = true;
        self.warning = None;
        self.refresh_send(cx);
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let result = client.command(command).await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.sending = false;
                if key.is_some() && !result.as_ref().is_ok_and(accepted) {
                    this.drafts
                        .entry(key.clone())
                        .or_default()
                        .unconfirmed_send();
                }
                match result {
                    Ok(reply) if accepted(&reply) => {
                        // 现场检查 IME，避免清掉尚未提交的 preedit。
                        let current = this.composer.update(cx, |c, cx| c.snapshot(window, cx));
                        let same_view = this.draft_key() == key;
                        if same_view && !current.composing {
                            this.drafts
                                .entry(key.clone())
                                .or_default()
                                .edit(current.text);
                        }
                        let mut follow_created = false;
                        if key.is_some() {
                            if let CommandReply::Receipt {
                                receipt: Receipt::Done { value },
                            } = &reply
                                && let Ok(remote) =
                                    serde_json::from_value::<nd_wire::Draft>(value["draft"].clone())
                            {
                                this.drafts.entry(key.clone()).or_default().sent(
                                    revision,
                                    remote,
                                    same_view && current.composing,
                                );
                            }
                        } else if !same_view || !current.composing {
                            follow_created =
                                this.drafts.entry(key.clone()).or_default().accept(revision);
                        }
                        if same_view {
                            this.restore_draft(window, cx);
                            if follow_created
                                && let CommandReply::Receipt {
                                    receipt:
                                        Receipt::Accepted {
                                            stream: Some(stream),
                                            ..
                                        },
                                } = reply
                                && let Some(session) = stream.strip_prefix("session/")
                            {
                                this.select_session(session.into(), window, cx);
                            }
                        }
                    }
                    Ok(CommandReply::DeliveryUnknown) => {
                        this.warning =
                            Some("交付不明，正文已保留；请核对记录后修改草稿或重试保存".into())
                    }
                    Ok(CommandReply::Receipt {
                        receipt: Receipt::Rejected { code, now },
                    }) => this.warning = Some(format!("未受理：{code} {now}")),
                    Ok(reply) => this.warning = Some(format!("未确认受理，正文已保留：{reply:?}")),
                    Err(e) => this.warning = Some(format!("发送未确认，正文已保留：{e}")),
                }
                let sessions: Vec<_> = this.drafts.keys().flatten().cloned().collect();
                for session in sessions {
                    this.persist_draft(session, window, cx);
                }
                this.refresh_send(cx);
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn chat_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.theme;
        let rows = self
            .snapshot
            .as_ref()
            .map(|s| sidebar(s, &self.state))
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(t.spacing.small))
            .child(
                div()
                    .id("new-session")
                    .cursor_pointer()
                    .text_color(rgba(t.colors.accent))
                    .child("＋ 新建会话")
                    .on_click(cx.listener(|this, _, window, cx| this.begin_create(window, cx))),
            )
            .when(rows.is_empty(), |d| d.child("暂无会话"))
            .children(rows.into_iter().map(|row| {
                let selected = row.selected;
                let target = row.session;
                div()
                    .id(SharedString::from(row.id))
                    .p(px(t.spacing.small))
                    .rounded(px(t.radius))
                    .bg(rgba(if selected {
                        t.colors.surface
                    } else {
                        t.colors.background
                    }))
                    .flex()
                    .flex_col()
                    .gap(px(t.spacing.small))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(session) = &target {
                            this.select_session(session.clone(), window, cx);
                        }
                    }))
                    .child(row.title)
                    .child(
                        div()
                            .text_size(px(t.typography.small))
                            .text_color(rgba(t.colors.muted))
                            .child(format!("{} {}", row.model, row.status)),
                    )
                    .when(!row.detail.is_empty(), |d| d.child(row.detail))
            }))
            .into_any_element()
    }
    pub(crate) fn chat_content(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.theme;
        if self.creating {
            return div()
                .flex()
                .flex_col()
                .gap(px(t.spacing.medium))
                .child(
                    div()
                        .text_size(px(t.typography.title))
                        .child("新建 Claude 会话"),
                )
                .child("工作目录")
                .child(Input::new(&self.directory))
                .child(
                    div()
                        .id("load-models")
                        .cursor_pointer()
                        .text_color(rgba(t.colors.accent))
                        .child(if self.model_loading {
                            "正在获取模型…"
                        } else {
                            "获取后端模型列表"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.model_loading {
                                this.load_models(cx);
                            }
                        })),
                )
                .children(self.models.iter().map(|model| {
                    let value = model.value.clone();
                    let disabled = model.disabled;
                    let chosen = self.model.as_ref() == Some(&model.value);
                    div()
                        .id(SharedString::from(format!("model/{}", model.value)))
                        .p(px(t.spacing.small))
                        .rounded(px(t.radius))
                        .bg(rgba(t.colors.surface))
                        .cursor_pointer()
                        .text_color(rgba(if disabled {
                            t.colors.muted
                        } else if chosen {
                            t.colors.accent
                        } else {
                            t.colors.foreground
                        }))
                        .child(format!(
                            "{} {}{}",
                            if chosen { "●" } else { "○" },
                            model.label,
                            if disabled { "（不可用）" } else { "" }
                        ))
                        .child(
                            div()
                                .text_size(px(t.typography.small))
                                .child(model.description.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !disabled {
                                this.model = Some(value.clone());
                                this.refresh_send(cx);
                                cx.notify();
                            }
                        }))
                }))
                .child("选择模型后，在下方输入第一条消息并发送。")
                .into_any_element();
        }
        let Some(snapshot) = &self.session_snapshot else {
            return div().child("正在读取会话…").into_any_element();
        };
        let view = conversation(snapshot);
        let mut items = Vec::new();
        for message in view.messages {
            let renderers = self.slots.values(&Slot::Item(message.kind.clone()));
            if let Some(render) = renderers.first() {
                let item = nd_view_model::ItemView {
                    id: message.id,
                    kind: message.kind,
                    title: message.title,
                    text: message.text,
                    selected: false,
                };
                items.push(render(
                    &Presentation {
                        snapshot: Some(snapshot),
                        state: &self.state,
                        theme: t,
                        item: Some(&item),
                    },
                    window,
                    cx,
                ));
                continue;
            }
            let body = if message.markdown {
                TextView::markdown(
                    SharedString::from(format!("{}/{}", snapshot.stream, message.id)),
                    message.text,
                )
                .selectable(true)
                .on_link_click(|url, _, _, cx| {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        cx.open_url(url);
                    }
                })
                .style(TextViewStyle {
                    paragraph_gap: rems(t.spacing.medium / t.typography.body),
                    heading_base_font_size: px(t.typography.title),
                    code_block: StyleRefinement::default()
                        .bg(rgba(t.colors.background))
                        .p(px(t.spacing.small))
                        .rounded(px(t.radius))
                        .font_family(t.typography.mono_family.clone())
                        .text_size(px(t.typography.body)),
                    ..Default::default()
                })
                .into_any_element()
            } else {
                div().child(message.text).into_any_element()
            };
            items.push(
                div()
                    .id(SharedString::from(message.id))
                    .flex()
                    .flex_col()
                    .gap(px(t.spacing.small))
                    .p(px(t.spacing.medium))
                    .rounded(px(t.radius))
                    .bg(rgba(t.colors.surface))
                    .child(
                        div()
                            .text_size(px(t.typography.small))
                            .text_color(rgba(t.colors.muted))
                            .child(format!("{} {}", message.title, message.status)),
                    )
                    .child(body)
                    .when(message.withdraw.is_some(), |d| {
                        let id = message.withdraw.clone().unwrap();
                        d.child(
                            div()
                                .id(SharedString::from(format!("withdraw/{id}")))
                                .text_color(rgba(t.colors.accent))
                                .cursor_pointer()
                                .child("撤回到输入框")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.withdraw_message(id.clone(), window, cx)
                                })),
                        )
                    })
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(t.spacing.medium))
            .child(div().text_color(rgba(t.colors.muted)).child(view.header))
            .children(items)
            .into_any_element()
    }
}
