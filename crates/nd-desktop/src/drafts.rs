use crate::Desktop;
use gpui_kit::prelude::*;
use gpui_kit::*;
use nd_wire::{CommandReply, Receipt};

impl Desktop {
    pub(crate) fn sync_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.draft_key() else {
            return;
        };
        let Some(remote) = self
            .session_snapshot
            .as_ref()
            .and_then(nd_view_model::session_draft)
        else {
            return;
        };
        let current = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
        let draft = self.drafts.entry(Some(session)).or_default();
        // Kit 的 Change 通知可能尚未处理，覆盖前仍须采样真实编辑缓冲。
        if !current.composing {
            draft.edit(current.text);
        }
        draft.observe(remote, current.composing || self.sending);
        let attachments = draft.attachments().to_vec();
        for a in attachments {
            self.load_attachment_image(&a, cx);
        }
        self.restore_draft(window, cx);
    }

    pub(crate) fn persist_draft(
        &mut self,
        session: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sending || self.draft_writes.contains(&session) {
            return;
        }
        let key = Some(session.clone());
        let Some(command) = self.drafts.entry(key.clone()).or_default().save_command(
            &session,
            &self.device,
            &uuid::Uuid::new_v4().to_string(),
        ) else {
            return;
        };
        self.draft_writes.insert(session.clone());
        let query_receipt = self.drafts[&key].needs_receipt();
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let mut missing = false;
            let result = if query_receipt {
                client.receipt(command.id).await.map(|lookup| match lookup {
                    nd_wire::ReceiptLookup::Found { receipt } => CommandReply::Receipt { receipt },
                    nd_wire::ReceiptLookup::Conflict => CommandReply::Conflict,
                    nd_wire::ReceiptLookup::Expired => CommandReply::Expired,
                    nd_wire::ReceiptLookup::Other => CommandReply::Other,
                    nd_wire::ReceiptLookup::Missing => {
                        missing = true;
                        CommandReply::DeliveryUnknown
                    }
                    nd_wire::ReceiptLookup::Unavailable { .. } => CommandReply::DeliveryUnknown,
                })
            } else {
                client.command(command).await
            };
            let _ = weak.update_in(cx, |this, window, cx| {
                this.draft_writes.remove(&session);
                let terminal = missing
                    || matches!(
                        &result,
                        Ok(CommandReply::Expired
                            | CommandReply::Conflict
                            | CommandReply::Receipt {
                                receipt: Receipt::Rejected { .. }
                            })
                    );
                let uncertain = query_receipt
                    || matches!(
                        &result,
                        Ok(CommandReply::DeliveryUnknown
                            | CommandReply::Other
                            | CommandReply::Receipt { .. })
                    );
                let saved = match result {
                    Ok(CommandReply::Receipt {
                        receipt: Receipt::Done { value },
                    }) => serde_json::from_value::<nd_wire::DraftUpdated>(value)
                        .map_err(|e| e.to_string()),
                    other => Err(format!("{other:?}")),
                };
                match saved {
                    Ok(saved) => {
                        let same_view = this.draft_key() == key;
                        let mut composing = false;
                        if same_view {
                            let current = this.composer.update(cx, |c, cx| c.snapshot(window, cx));
                            composing = current.composing;
                            if !composing {
                                this.drafts
                                    .entry(key.clone())
                                    .or_default()
                                    .edit(current.text);
                            }
                        }
                        if saved.saved.is_some() {
                            this.warning =
                                Some("其他界面已更新草稿；本次编辑已另存，可在另存稿中取回".into());
                        }
                        this.drafts
                            .entry(key.clone())
                            .or_default()
                            .saved(saved, composing);
                        if same_view {
                            this.restore_draft(window, cx);
                        }
                        if let Some((target, text, revision, intent)) = this.queued_send.clone()
                            && target == session
                            && this.drafts[&key].is_saved()
                        {
                            this.queued_send = None;
                            this.refresh_send(cx);
                            if same_view && this.drafts[&key].revision() == revision {
                                this.send_text_intent(text, intent, window, cx);
                            } else {
                                this.warning = Some("草稿已变化，请核对后重新发送".into());
                            }
                        }
                        this.persist_draft(session.clone(), window, cx);
                    }
                    Err(e) => {
                        let draft = this.drafts.entry(key.clone()).or_default();
                        if terminal {
                            draft.save_rejected();
                        } else {
                            draft.save_failed(uncertain);
                        }
                        this.queued_send = None;
                        this.warning =
                            Some(format!("草稿保存未确认，文字仍在本窗口；可重试保存：{e}"));
                        if missing {
                            this.persist_draft(session.clone(), window, cx);
                        }
                    }
                }
                this.refresh_send(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_saved_draft(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .composer
            .update(cx, |c, cx| c.snapshot(window, cx))
            .composing
        {
            self.warning = Some("请先完成组词，再载入另存稿".into());
            cx.notify();
            return;
        }
        let Some(saved) = self
            .session_snapshot
            .as_ref()
            .and_then(nd_view_model::session_draft)
            .and_then(|d| d.saved.into_iter().find(|s| s.id == id))
        else {
            return;
        };
        let Some(session) = self.draft_key() else {
            return;
        };
        let draft = self.drafts.entry(Some(session.clone())).or_default();
        draft.edit(saved.text);
        draft.replace_attachments(saved.attachments);
        self.queued_send = None;
        self.restore_draft(window, cx);
        self.persist_draft(session, window, cx);
        cx.notify();
    }

    pub(crate) fn draft_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let current = self.composer.update(cx, |c, cx| c.snapshot(window, cx));
        let t = &self.theme;
        let saved = self
            .session_snapshot
            .as_ref()
            .and_then(nd_view_model::session_draft)
            .map(|d| d.saved)
            .unwrap_or_default();
        let ready = self
            .drafts
            .get(&self.draft_key())
            .is_some_and(|d| d.is_saved() && d.text() == current.text);
        div()
            .id("drafts")
            .max_h(px(t.typography.body * 8.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(t.spacing.small))
            .text_size(px(t.typography.small))
            .text_color(rgba(t.colors.muted))
            .child(if ready {
                "草稿已保存"
            } else {
                "草稿待保存"
            })
            .when(!ready, |d| {
                d.child(
                    div()
                        .id("retry-draft")
                        .cursor_pointer()
                        .text_color(rgba(t.colors.accent))
                        .child("重试保存草稿")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(session) = this.draft_key() {
                                this.drafts
                                    .entry(Some(session.clone()))
                                    .or_default()
                                    .retry_save();
                                this.persist_draft(session, window, cx);
                            }
                        })),
                )
            })
            .children(saved.into_iter().map(|saved| {
                let id = saved.id;
                div()
                    .flex()
                    .flex_col()
                    .gap(px(t.spacing.small))
                    .p(px(t.spacing.small))
                    .bg(rgba(t.colors.surface))
                    .rounded(px(t.radius))
                    .child("另存的草稿")
                    .child(saved.text)
                    .children(
                        saved
                            .attachments
                            .iter()
                            .map(|a| div().child(format!("📎 {}", a.name))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("restore/{id}")))
                            .cursor_pointer()
                            .text_color(rgba(t.colors.accent))
                            .child("载入这份草稿")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.restore_saved_draft(&id, window, cx)
                            })),
                    )
            }))
            .into_any_element()
    }

    #[cfg(feature = "scenarios")]
    pub fn scenario_draft(plan: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |weak, cx| {
            let mut drop_stage = 0;
            let mut edited = false;
            for _ in 0..400 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                if weak
                    .update_in(cx, |this, window, cx| {
                        if this.session_snapshot.is_none() {
                            return false;
                        }
                        if let Some(gate) = plan["edit_gate"].as_str()
                            && !std::path::Path::new(gate).exists()
                        {
                            return false;
                        }
                        if edited {
                            if this
                                .drafts
                                .get(&this.draft_key())
                                .is_some_and(|d| d.is_saved())
                            {
                                this.composer.update(cx, |c, cx| c.submit(window, cx));
                                return true;
                            }
                            return false;
                        }
                        if let Some(file) = plan["file"].as_str() {
                            if drop_stage < 2 {
                                let size = window.viewport_size();
                                let position = point(
                                    size.width / 2.,
                                    size.height - px(this.theme.spacing.large * 3.),
                                );
                                let event = if drop_stage == 0 {
                                    FileDropEvent::Entered {
                                        position,
                                        paths: ExternalPaths(
                                            [std::path::PathBuf::from(file)].into_iter().collect(),
                                        ),
                                    }
                                } else {
                                    FileDropEvent::Submit { position }
                                };
                                window.defer(cx, move |window, cx| {
                                    window.dispatch_event(PlatformInput::FileDrop(event), cx);
                                });
                                drop_stage += 1;
                                return false;
                            }
                            if this.uploading > 0
                                || this
                                    .drafts
                                    .get(&this.draft_key())
                                    .is_none_or(|d| d.attachments().len() != 1)
                            {
                                return false;
                            }
                        }
                        if let Some(id) = plan["restore"].as_str() {
                            this.restore_saved_draft(id, window, cx);
                        } else {
                            let input = this.composer.read(cx).editor().clone();
                            input.update(cx, |input, cx| {
                                input.set_value(plan["text"].as_str().unwrap(), window, cx);
                                input.focus(window, cx);
                            });
                            if plan["send"] == true {
                                this.composer.update(cx, |c, cx| c.submit(window, cx));
                            }
                        }
                        if plan["send_when_saved"] == true {
                            edited = true;
                            false
                        } else {
                            true
                        }
                    })
                    .unwrap()
                {
                    return;
                }
            }
            panic!("draft scenario never received its session");
        })
        .detach();
    }
}
