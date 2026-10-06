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
    text: String,
    revision: u64,
    server_version: Option<u64>,
    server_text: String,
}
impl Draft {
    pub fn server_version(&self) -> Option<u64> {
        self.server_version
    }
    /// 撤回请求携带当前编辑缓冲；随后的同文快照不是另一份待追加正文。
    pub fn preparing_restore(&mut self) {
        self.server_text = self.text.clone();
    }
    pub fn receive(&mut self, version: u64, text: &str) -> bool {
        if self.server_version.is_some_and(|known| version <= known) {
            return false;
        }
        let before = self.text.clone();
        if self.text == self.server_text || self.text == text {
            self.edit(text.into());
        } else if !text.is_empty() {
            let added = text
                .strip_prefix(&self.server_text)
                .unwrap_or(text)
                .trim_start_matches('\n');
            if !added.is_empty() {
                let mut merged = self.text.clone();
                if !merged.is_empty() {
                    merged.push_str("\n\n");
                }
                merged.push_str(added);
                self.edit(merged);
            }
        }
        self.server_text = text.into();
        self.server_version = Some(version);
        self.text != before
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn edit(&mut self, text: String) {
        if self.text != text {
            self.text = text;
            self.revision += 1;
        }
    }
    pub fn accept(&mut self, revision: u64) -> bool {
        if revision != self.revision {
            return false;
        }
        self.edit(String::new());
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
