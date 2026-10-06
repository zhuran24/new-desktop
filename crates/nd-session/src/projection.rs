//! 对话投影：把会话里要显示的东西（`Shown`）变成 nd-wire 条目。
//!
//! 两份实现，输入相同、结果必须相同：[`project`] 是最简版，每次从头扫一遍全部输入；
//! [`Projection`] 是执行器用的增量版。最简版保留下来做差分基准：以后换更快的实现，
//! 用同一组输入（录制回放出来的）比对两边的结果。
use nd_backend::{Item as BackendItem, ItemKind};
use nd_wire::{Fallback, Item};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const NAMESPACE: &str = "session";

/// 一件要显示的事。同一个条目 id 的后一件覆盖前一件；增量（`Delta`）只累加文字，
/// 完整条目一到就整体替换，之后迟到的增量不再改它。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shown", rename_all = "snake_case")]
pub enum Shown {
    Draft {
        draft: nd_wire::Draft,
    },
    Lineage {
        data: Value,
    },
    Header {
        data: Value,
    },
    Prompt {
        #[serde(default)]
        attachments: Vec<nd_wire::Attachment>,
        id: String,
        text: String,
        intent: String,
        state: String,
        native: Option<String>,
        reason: Option<String>,
    },
    Block {
        item: BackendItem,
    },
    Delta {
        item: String,
        kind: ItemKind,
        text: String,
    },
    Turn {
        carrier: String,
        n: u64,
        ok: bool,
        subtype: String,
        error: Option<String>,
    },
    Op {
        id: String,
        kind: String,
        phase: String,
        reason: Option<String>,
        irreversible: Vec<String>,
    },
    Asked {
        id: String,
        kind: String,
        raw: Value,
    },
}

fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Text => "text",
        ItemKind::Thinking => "thinking",
        ItemKind::ToolUse => "tool_use",
        ItemKind::ToolResult => "tool_result",
        ItemKind::Other => "other",
    }
}

fn shorten(text: &str) -> String {
    let mut out: String = text.chars().take(400).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

impl Shown {
    /// 条目 id；增量与它的完整条目同 id。
    pub fn id(&self) -> String {
        match self {
            Shown::Draft { .. } => "draft".into(),
            Shown::Lineage { .. } => "lineage".into(),
            Shown::Header { .. } => "header".into(),
            Shown::Prompt { id, .. } => format!("prompt/{id}"),
            Shown::Block { item } => format!("block/{}", item.id),
            Shown::Delta { item, .. } => format!("block/{item}"),
            Shown::Turn { carrier, n, .. } => format!("turn/{carrier}/{n}"),
            Shown::Op { id, .. } => format!("op/{id}"),
            Shown::Asked { id, .. } => format!("asked/{id}"),
        }
    }
    pub fn is_delta(&self) -> bool {
        matches!(self, Shown::Delta { .. })
    }
    /// 渲染成条目；增量的累积文字由调用方给。
    fn render(&self, seq: u64, accumulated: Option<&str>) -> Item {
        let id = self.id();
        let (kind, mut data, title, text) = match self {
            Shown::Draft { draft } => (
                "draft",
                serde_json::to_value(draft).expect("draft value"),
                "草稿".to_owned(),
                shorten(&draft.text),
            ),
            Shown::Lineage { data } => (
                "lineage",
                data.clone(),
                "对话谱系".to_owned(),
                format!("{} 轮", data["rounds"].as_array().map_or(0, Vec::len)),
            ),
            Shown::Header { data } => (
                "header",
                data.clone(),
                "会话".to_owned(),
                format!(
                    "{}（{}）",
                    data["status"].as_str().unwrap_or("?"),
                    data["cwd"].as_str().unwrap_or("")
                ),
            ),
            Shown::Prompt {
                attachments,
                id,
                text,
                intent,
                state,
                native,
                reason,
            } => (
                "prompt",
                json!({"message":id,"text":text,"attachments":attachments,"intent":intent,"state":state,"native":native,"reason":reason}),
                "你".to_owned(),
                format!("{}［{state}］", shorten(text)),
            ),
            Shown::Block { item } => (
                kind_name(item.kind),
                json!({"item":item.id,"text":item.text,"complete":true,"raw":item.raw}),
                "Claude".to_owned(),
                shorten(&item.text),
            ),
            Shown::Delta { item, kind, .. } => {
                let text = accumulated.unwrap_or_default();
                (
                    kind_name(*kind),
                    json!({"item":item,"text":text,"complete":false}),
                    "Claude".to_owned(),
                    shorten(text),
                )
            }
            Shown::Turn {
                carrier,
                n,
                ok,
                subtype,
                error,
            } => (
                "turn",
                json!({"carrier":carrier,"n":n,"ok":ok,"subtype":subtype,"error":error}),
                "回合结束".to_owned(),
                if *ok {
                    "回合结束".to_owned()
                } else {
                    format!("回合出错：{}", error.clone().unwrap_or(subtype.clone()))
                },
            ),
            Shown::Op {
                id,
                kind,
                phase,
                reason,
                irreversible,
            } => (
                "op",
                json!({"op":id,"kind":kind,"phase":phase,"reason":reason,"irreversible":irreversible}),
                format!("操作 {kind}"),
                match reason {
                    Some(reason) => format!("{phase}：{reason}"),
                    None => phase.clone(),
                },
            ),
            Shown::Asked { id, kind, raw } => (
                "asked",
                json!({"request":id,"subtype":kind,"raw":raw}),
                "后端在等回答".to_owned(),
                format!("{kind}（这一版界面还不能回答）"),
            ),
        };
        data["seq"] = json!(seq);
        Item {
            id,
            namespace: NAMESPACE.into(),
            kind: kind.into(),
            data,
            fallback: Fallback { title, text },
        }
    }
}

/// 最简版：从头扫一遍全部输入。每个条目取最后一件完整的事；没有完整的就把增量连起来。
/// 序号是条目第一次出现的先后。
pub fn project(log: &[Shown]) -> Vec<Item> {
    let mut order: Vec<String> = vec![];
    for shown in log {
        let id = shown.id();
        if !order.contains(&id) {
            order.push(id);
        }
    }
    let mut items = vec![];
    for (index, id) in order.iter().enumerate() {
        let seq = index as u64 + 1;
        let same: Vec<&Shown> = log.iter().filter(|s| &s.id() == id).collect();
        let item = match same.iter().rev().find(|s| !s.is_delta()) {
            Some(last) => last.render(seq, None),
            None => {
                let text: String = same
                    .iter()
                    .filter_map(|s| match s {
                        Shown::Delta { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                same.last().unwrap().render(seq, Some(&text))
            }
        };
        items.push(item);
    }
    items
}

/// 增量版：执行器每来一件事改一个条目。
#[derive(Clone, Debug, Default)]
pub struct Projection {
    items: BTreeMap<String, (u64, Item, bool)>,
    streaming: BTreeMap<String, String>,
    next_seq: u64,
}
impl Projection {
    /// 从显示缓存恢复；检查点同时保存仍在生成中的累积正文。
    pub fn restore(items: Vec<(u64, Item)>) -> Self {
        let next_seq = items.iter().map(|(s, _)| *s).max().unwrap_or(0);
        let streaming = items
            .iter()
            .filter(|(_, i)| i.data["complete"] == false)
            .map(|(_, i)| {
                (
                    i.id.clone(),
                    i.data["text"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        Self {
            items: items
                .into_iter()
                .map(|(seq, item)| {
                    let complete = item.data["complete"] != false;
                    (item.id.clone(), (seq, item, complete))
                })
                .collect(),
            streaming,
            next_seq,
        }
    }
    /// 应用一件事，返回变了的条目（`None` 表示没变）。
    pub fn apply(&mut self, shown: &Shown) -> Option<(u64, Item)> {
        let id = shown.id();
        let seq = match self.items.get(&id) {
            Some((seq, _, _)) => *seq,
            None => {
                self.next_seq += 1;
                self.next_seq
            }
        };
        let item = match shown {
            Shown::Delta { text, .. } => {
                if self
                    .items
                    .get(&id)
                    .is_some_and(|(_, _, complete)| *complete)
                {
                    return None;
                }
                let acc = self.streaming.entry(id.clone()).or_default();
                acc.push_str(text);
                let item = shown.render(seq, Some(acc));
                self.items.insert(id, (seq, item.clone(), false));
                return Some((seq, item));
            }
            other => other.render(seq, None),
        };
        self.streaming.remove(&id);
        if self.items.get(&id).is_some_and(|(_, old, _)| old == &item) {
            return None;
        }
        self.items.insert(id, (seq, item.clone(), true));
        Some((seq, item))
    }
    pub fn items_with_seq(&self) -> Vec<(u64, Item)> {
        let mut items: Vec<_> = self
            .items
            .values()
            .map(|(seq, item, _)| (*seq, item.clone()))
            .collect();
        items.sort_by_key(|(seq, _)| *seq);
        items
    }
    pub fn items(&self) -> Vec<Item> {
        let mut items: Vec<_> = self.items.values().collect();
        items.sort_by_key(|(seq, _, _)| *seq);
        items.into_iter().map(|(_, item, _)| item.clone()).collect()
    }
}
