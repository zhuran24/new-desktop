use crate::ViewState;
use nd_wire::Snapshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub session: Option<String>,
    pub title: String,
    pub model: String,
    pub status: String,
    pub detail: String,
    pub selected: bool,
}

pub fn sidebar(snapshot: &Snapshot, state: &ViewState) -> Vec<SessionRow> {
    snapshot
        .items
        .iter()
        .filter(|item| {
            item.namespace == "sessions" && matches!(item.kind.as_str(), "session" | "notice")
        })
        .map(|item| {
            let session = (item.kind == "session")
                .then(|| item.data["session"].as_str().map(str::to_owned))
                .flatten();
            SessionRow {
                selected: session.is_some() && session == state.selected_session,
                id: item.id.clone(),
                session,
                title: item.data["cwd"]
                    .as_str()
                    .unwrap_or(&item.fallback.title)
                    .into(),
                model: item.data["model"].as_str().unwrap_or_default().into(),
                status: match item.data["status"].as_str() {
                    Some("preparing") => "准备中",
                    Some("partial") => "部分完成",
                    Some("withdrawn") => "创建失败",
                    Some("active") => "可对话",
                    _ => &item.fallback.text,
                }
                .into(),
                detail: item.data["reason"]
                    .as_str()
                    .or_else(|| item.data["note"].as_str())
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageView {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub text: String,
    pub status: String,
    pub markdown: bool,
    pub withdraw: Option<String>,
    pub blocks: Vec<crate::MessageBlock>,
    pub attachments: Vec<nd_wire::Attachment>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationView {
    pub messages: Vec<MessageView>,
    pub header: String,
    pub can_send: bool,
}
/// 累积正文整体投影；未知条目显示后备文字，不解析 CLI 流水。
pub fn conversation(snapshot: &Snapshot) -> ConversationView {
    let header = snapshot.items.iter().find(|i| i.kind == "header");
    let mut items: Vec<_> = snapshot
        .items
        .iter()
        .filter(|i| {
            !matches!(i.kind.as_str(), "header" | "draft")
                && !(i.kind == "op" && i.data["phase"] == "done")
        })
        .collect();
    items.sort_by_key(|i| i.data["seq"].as_u64().unwrap_or(u64::MAX));
    ConversationView {
        can_send: header
            .is_some_and(|i| matches!(i.data["status"].as_str(), Some("active" | "preparing"))),
        header: header
            .map(|i| {
                let status = match i.data["status"].as_str() {
                    Some("preparing") => "准备中",
                    Some("partial") => "部分完成",
                    Some("withdrawn") => "创建失败",
                    Some("active") => {
                        if i.data["process"]["turn_running"] == true {
                            "正在回复"
                        } else {
                            "可对话"
                        }
                    }
                    _ => &i.fallback.text,
                };
                let mut label = format!(
                    "{status} · {} · {}",
                    i.data["cwd"].as_str().unwrap_or_default(),
                    i.data["model"].as_str().unwrap_or_default()
                );
                if let Some(note) = i.data["note"].as_str() {
                    label.push_str(&format!(" · {note}"));
                }
                label
            })
            .unwrap_or_default(),
        messages: items
            .into_iter()
            .map(|i| MessageView {
                id: i.id.clone(),
                kind: i.kind.clone(),
                title: if i.kind == "op" {
                    match i.data["kind"].as_str() {
                        Some("create") => "新建会话",
                        Some("launch") => "连接会话",
                        Some("reclaim") => "回收闲置进程",
                        _ => &i.fallback.title,
                    }
                    .into()
                } else {
                    i.fallback.title.clone()
                },
                text: if i.kind == "op" {
                    i.data["reason"]
                        .as_str()
                        .unwrap_or(match i.data["phase"].as_str() {
                            Some("running") => "进行中",
                            Some("compensated") => "已撤销",
                            Some("partial") => "部分完成",
                            Some("unresolved") => "等待处理",
                            Some("rejected") => "未受理",
                            _ => &i.fallback.text,
                        })
                } else {
                    i.data["text"].as_str().unwrap_or(&i.fallback.text)
                }
                .into(),
                markdown: i.kind == "text",
                withdraw: (header.is_some_and(|h| h.data["interaction"]["withdraw"] == true)
                    && i.kind == "prompt"
                    && matches!(
                        i.data["state"].as_str(),
                        Some("held" | "waiting" | "pending" | "written" | "queued")
                    ))
                .then(|| i.data["message"].as_str().map(str::to_owned))
                .flatten(),
                blocks: crate::message_blocks(
                    &i.kind,
                    i.data["text"].as_str().unwrap_or(&i.fallback.text),
                    &i.data["raw"],
                ),
                attachments: serde_json::from_value(i.data["attachments"].clone())
                    .unwrap_or_default(),
                status: if i.kind == "prompt" {
                    match i.data["state"].as_str() {
                        Some("held") => "代持中",
                        Some("waiting") => "等待可写",
                        Some("pending") => "等待写出",
                        Some("written") => "已写出",
                        Some("queued") => "排队中",
                        Some("withdrawing") => "撤回中",
                        Some("withdrawn") => "已撤回",
                        Some("landed") => "已送达",
                        Some("failed") => "发送失败",
                        Some("unknown") => "交付不明",
                        _ => "",
                    }
                } else if i.data["complete"] == false {
                    "生成中"
                } else {
                    ""
                }
                .into(),
            })
            .collect(),
    }
}
/// 只有明确受理的收据才能清草稿；交付不明、冲突、拒绝均保留。
pub fn accepted(reply: &nd_wire::CommandReply) -> bool {
    matches!(
        reply,
        nd_wire::CommandReply::Receipt {
            receipt: nd_wire::Receipt::Accepted { .. } | nd_wire::Receipt::Done { .. }
        }
    )
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    attachments: Vec<nd_wire::Attachment>,
    text: String,
    revision: u64,
    base: u64,
    remote: Option<nd_wire::Draft>,
    dirty: bool,
    pending: Option<(u64, nd_wire::Command)>,
    save_unconfirmed: bool,
    send_unconfirmed: bool,
}

pub fn session_draft(snapshot: &Snapshot) -> Option<nd_wire::Draft> {
    snapshot
        .items
        .iter()
        .find(|i| i.id == "draft" && i.kind == "draft")
        .and_then(|i| serde_json::from_value(i.data.clone()).ok())
}
impl Draft {
    pub fn attachments(&self) -> &[nd_wire::Attachment] {
        &self.attachments
    }
    pub fn attach(&mut self, attachment: nd_wire::Attachment) {
        if !self.attachments.contains(&attachment) {
            self.attachments.push(attachment);
            self.revision += 1;
            self.dirty = true;
            self.send_unconfirmed = false;
        }
    }
    pub fn detach(&mut self, index: usize) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
            self.revision += 1;
            self.dirty = true;
            self.send_unconfirmed = false;
        }
    }
    pub fn replace_attachments(&mut self, attachments: Vec<nd_wire::Attachment>) {
        if self.attachments != attachments {
            self.attachments = attachments;
            self.revision += 1;
            self.dirty = true;
            self.send_unconfirmed = false;
        }
    }
    pub fn version(&self) -> u64 {
        self.base
    }
    pub fn is_saved(&self) -> bool {
        self.remote.is_some() && !self.dirty && self.pending.is_none() && !self.send_unconfirmed
    }
    pub fn needs_send_review(&self) -> bool {
        self.send_unconfirmed
    }
    pub fn needs_receipt(&self) -> bool {
        self.save_unconfirmed
    }
    pub fn save_failed(&mut self, uncertain: bool) {
        self.save_unconfirmed = uncertain;
    }
    pub fn unconfirmed_send(&mut self) {
        self.send_unconfirmed = true;
    }
    pub fn retry_save(&mut self) {
        if self.send_unconfirmed {
            self.send_unconfirmed = false;
            self.dirty = true;
        }
    }
    /// 只读副本更新不能盖掉本地未持久化的编辑或组词。
    pub fn observe(&mut self, remote: nd_wire::Draft, composing: bool) {
        if self
            .remote
            .as_ref()
            .is_none_or(|old| remote.version >= old.version)
        {
            self.remote = Some(remote);
        }
        self.adopt_remote(composing);
    }
    fn adopt_remote(&mut self, composing: bool) {
        if !composing
            && !self.send_unconfirmed
            && !self.dirty
            && self.pending.is_none()
            && let Some(remote) = &self.remote
        {
            if self.text != remote.text || self.attachments != remote.attachments {
                self.text.clone_from(&remote.text);
                self.attachments.clone_from(&remote.attachments);
                self.revision += 1;
            }
            self.base = remote.version;
        }
    }
    /// 每会话至多一个在途保存；未知结果保留原命令，needs_receipt 时只能查收据。
    pub fn save_command(
        &mut self,
        session: &str,
        device: &str,
        id: &str,
    ) -> Option<nd_wire::Command> {
        if let Some((_, command)) = &self.pending {
            return Some(command.clone());
        }
        if !self.dirty || self.remote.is_none() || self.send_unconfirmed {
            return None;
        }
        let command = nd_wire::Command {
            id: id.into(),
            device: device.into(),
            name: "session.draft.update".into(),
            args: serde_json::json!(nd_wire::DraftUpdate {
                session: session.into(),
                text: self.text.clone(),
                attachments: self.attachments.clone(),
            }),
            expect: serde_json::json!(nd_wire::DraftExpected {
                draft_version: self.base
            }),
        };
        self.pending = Some((self.revision, command.clone()));
        Some(command)
    }
    pub fn saved(&mut self, result: nd_wire::DraftUpdated, composing: bool) {
        self.save_unconfirmed = false;
        if let Some((revision, _)) = self.pending.take() {
            if revision == self.revision {
                self.dirty = false;
            }
            // 成功后续写自己的新版本；落败后的继续编辑仍按原基准另存。
            if result.saved.is_none() {
                self.base = result.draft.version;
            }
        }
        self.observe(result.draft, composing);
    }
    /// 清稿来自发送事务的结果，不能再补发一条无条件清空命令。
    pub fn sent(&mut self, revision: u64, remote: nd_wire::Draft, composing: bool) {
        if revision == self.revision {
            self.dirty = false;
        }
        if remote.version == self.base + 1
            && remote.text.is_empty()
            && remote.attachments.is_empty()
        {
            self.base = remote.version;
        }
        self.observe(remote, composing);
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn edit(&mut self, text: String) {
        if self.text != text {
            self.send_unconfirmed = false;
            self.text = text;
            self.revision += 1;
            self.dirty = true;
        }
    }
    pub fn accept(&mut self, revision: u64) -> bool {
        if revision != self.revision {
            return false;
        }
        self.edit(String::new());
        if !self.attachments.is_empty() {
            self.attachments.clear();
            self.revision += 1;
        }
        true
    }
}

/// 输入法已经由 Composer 消费后，按面板、活动回合、空闲双 Esc 的顺序分派。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    None,
    ClosePanel,
    Interrupt,
    RewindMenu,
}
#[derive(Clone, Debug, Default)]
pub struct EscapeState {
    idle_at: Option<u64>,
}
impl EscapeState {
    pub fn press(
        &mut self,
        now_ms: u64,
        panel_open: bool,
        running: bool,
        can_rewind: bool,
    ) -> Escape {
        if panel_open {
            self.idle_at = None;
            return Escape::ClosePanel;
        }
        if running {
            self.idle_at = None;
            return Escape::Interrupt;
        }
        if !can_rewind {
            self.idle_at = None;
            return Escape::None;
        }
        if self
            .idle_at
            .take()
            .is_some_and(|t| now_ms.saturating_sub(t) <= 500)
        {
            return Escape::RewindMenu;
        }
        self.idle_at = Some(now_ms);
        Escape::None
    }
}
