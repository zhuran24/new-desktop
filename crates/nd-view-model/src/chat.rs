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
        .filter(|i| i.kind != "header" && !(i.kind == "op" && i.data["phase"] == "done"))
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
}
impl Draft {
    pub fn attachments(&self) -> &[nd_wire::Attachment] {
        &self.attachments
    }
    pub fn attach(&mut self, attachment: nd_wire::Attachment) {
        if !self.attachments.contains(&attachment) {
            self.attachments.push(attachment);
            self.revision += 1;
        }
    }
    pub fn detach(&mut self, index: usize) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
            self.revision += 1;
        }
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
        if !self.attachments.is_empty() {
            self.attachments.clear();
            self.revision += 1;
        }
        true
    }
}
