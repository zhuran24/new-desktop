use crate::Desktop;
use gpui_kit::{prelude::*, *};
use nd_ui_core::AttachmentSource;
use nd_wire::Attachment;
use std::sync::Arc;

impl Desktop {
    pub(crate) fn attachment_drop_target(
        &self,
        element: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let weak = cx.entity().downgrade();
        element
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    this.external_drop_paths = Some(event.drag(cx).clone());
                }),
            )
            .on_file_drop_exit(cx.listener(|this, _, _, _| {
                this.external_drop_paths = None;
            }))
            .relative()
            .child(
                canvas(
                    |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |_, hitbox, window, _| {
                        let weak = weak.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                            if !phase.capture() || event.button != MouseButton::Left {
                                return;
                            }
                            // GPUI 0.3.7 的 FileDrop 不更新输入模式。普通 on_drop 用
                            // is_hovered，键盘输入后会漏掉已完成的拖放；按实际落点命中。
                            let inside = hitbox.is_hovered_at(event.position, window);
                            let active = cx.has_active_drag();
                            let _ = weak.update(cx, |this, cx| {
                                let paths = this.external_drop_paths.take();
                                if inside
                                    && active
                                    && let Some(paths) = paths
                                {
                                    cx.stop_active_drag(window);
                                    this.upload_attachments(
                                        paths.0.into_iter().map(AttachmentSource::Path).collect(),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            });
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    pub(crate) fn upload_attachments(
        &mut self,
        sources: Vec<AttachmentSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.draft_key();
        let existing = self
            .drafts
            .entry(key.clone())
            .or_default()
            .attachments()
            .len();
        if sources.len() + existing + self.uploading > nd_wire::MAX_ATTACHMENTS_PER_MESSAGE {
            self.warning = Some("每条消息最多 8 个附件".into());
            cx.notify();
            return;
        }
        self.uploading += sources.len();
        self.refresh_send(cx);
        for source in sources {
            let client = self.client.clone();
            let key = key.clone();
            cx.spawn_in(window, async move |weak, cx| {
                let result = client.upload(source).await;
                let _ = weak.update_in(cx, |this, window, cx| {
                    this.uploading -= 1;
                    match result {
                        Ok(a) => {
                            this.load_attachment_image(&a, cx);
                            this.drafts.entry(key.clone()).or_default().attach(a);
                            this.queued_send = None;
                            if let Some(session) = key {
                                this.persist_draft(session, window, cx);
                            }
                        }
                        Err(e) => this.warning = Some(format!("附件上传失败：{e}")),
                    }
                    this.refresh_send(cx);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }
    pub(crate) fn load_attachment_image(&mut self, a: &Attachment, cx: &mut Context<Self>) {
        let format = match a.media_type.as_str() {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            "image/webp" => ImageFormat::Webp,
            "image/gif" => ImageFormat::Gif,
            _ => return,
        };
        if !self.images.request(a.blob.as_str()) {
            return;
        }
        let client = self.client.clone();
        let blob = a.blob.clone();
        cx.spawn(async move |weak, cx| {
            let result = client.blob(blob.clone()).await;
            let _ = weak.update(cx, |this, cx| {
                if let Ok(bytes) = result {
                    this.images
                        .complete(blob.as_str(), Arc::new(Image::from_bytes(format, bytes)));
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn attachment_view(&self, a: &Attachment, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.theme;
        let attachment = a.clone();
        let missing_image = a.media_type.starts_with("image/")
            && self.images.get(a.blob.as_str()).cloned().is_none();
        div()
            .id(SharedString::from(format!("{}/{}", a.blob, a.name)))
            .when(missing_image, |d| {
                d.cursor_pointer()
                    .child("点击加载图片或重试")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.images.remove(attachment.blob.as_str());
                        this.load_attachment_image(&attachment, cx);
                    }))
            })
            .flex()
            .flex_col()
            .gap(px(t.spacing.small))
            .text_color(rgba(t.colors.muted))
            .text_size(px(t.typography.small))
            .child(format!("📎 {} · {} 字节", a.name, a.size))
            .when_some(self.images.get(a.blob.as_str()).cloned(), |d, image| {
                d.child(
                    img(image)
                        .max_w(px(t.spacing.large * 10.))
                        .max_h(px(t.spacing.large * 6.))
                        .object_fit(ObjectFit::Contain),
                )
            })
            .into_any_element()
    }
    pub(crate) fn draft_attachments(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = &self.theme;
        let attachments = self
            .drafts
            .get(&self.draft_key())
            .map(|d| d.attachments())
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(t.spacing.small))
            .text_color(rgba(t.colors.muted))
            .text_size(px(t.typography.small))
            .child(if self.uploading > 0 {
                "正在上传附件…"
            } else {
                "可粘贴或拖入图片、PDF、UTF-8 文本（每个最多 5 MiB）"
            })
            .children(attachments.iter().enumerate().map(|(index, a)| {
                div()
                    .id(("attachment", index))
                    .flex()
                    .justify_between()
                    .child(self.attachment_view(a, cx))
                    .child(
                        div()
                            .id(("remove-attachment", index))
                            .cursor_pointer()
                            .child("移除")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.drafts
                                    .entry(this.draft_key())
                                    .or_default()
                                    .detach(index);
                                this.queued_send = None;
                                if let Some(session) = this.draft_key() {
                                    this.persist_draft(session, window, cx);
                                }
                                this.refresh_send(cx);
                                cx.notify();
                            })),
                    )
            }))
            .into_any_element()
    }
}
