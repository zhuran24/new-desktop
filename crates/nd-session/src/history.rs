//! 显示历史的纯投影：正文按需取页，导航只消费谱系确认的轮，不推算后端回合。
use nd_wire::{Fallback, Item, Page, PageReq};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const WINDOW: usize = 60;
fn control(item: &Item) -> bool {
    matches!(item.kind.as_str(), "header" | "draft" | "lineage")
}
#[derive(Default)]
pub struct History {
    stream: String,
    items: BTreeMap<u64, Item>,
    ids: BTreeMap<String, u64>,
}
impl History {
    pub fn new(stream: String) -> Self {
        Self {
            stream,
            ..Self::default()
        }
    }
    pub fn insert(&mut self, item: Item) {
        let seq = item.data["seq"].as_u64().expect("projection sequence");
        if let Some(old) = self.ids.insert(item.id.clone(), seq)
            && old != seq
        {
            self.items.remove(&old);
        }
        self.items.insert(seq, item);
    }
    pub fn get(&self, id: &str) -> Option<&Item> {
        self.ids.get(id).and_then(|s| self.items.get(s))
    }
    fn segment(&self) -> Value {
        self.get("lineage")
            .map_or(Value::Null, |i| i.data["current"].clone())
    }
    fn cursor(&self, seq: u64) -> String {
        serde_json::to_string(&(&self.stream, self.segment(), seq)).unwrap()
    }
    fn decode(&self, cursor: &str) -> Result<u64, String> {
        let (stream, segment, seq): (String, Value, u64) =
            serde_json::from_str(cursor).map_err(|_| "invalid_cursor")?;
        if stream != self.stream || segment != self.segment() || !self.items.contains_key(&seq) {
            return Err("invalid_cursor".into());
        }
        Ok(seq)
    }
    pub fn navigation(&self) -> Item {
        let rounds = self
            .get("lineage")
            .and_then(|i| i.data["rounds"].as_array());
        let rounds: Vec<_> = rounds.into_iter().flatten().map(|r| {
            let prompts: Vec<_> = r["messages"].as_array().into_iter().flatten()
                .filter_map(|m| self.get(&format!("prompt/{}", m.as_str()?))).collect();
            let anchor = prompts.iter().min_by_key(|i| i.data["seq"].as_u64()).map(|i| i.id.as_str());
            let preview: String = prompts.iter().flat_map(|i| {
                let mut parts = Vec::new();
                if let Some(text) = i.data["text"].as_str().filter(|text| !text.is_empty()) { parts.push(text.to_owned()); }
                for attachment in i.data["attachments"].as_array().into_iter().flatten() {
                    if let Some(name) = attachment["name"].as_str() { parts.push(format!("附件：{name}")); }
                }
                parts
            }).collect::<Vec<_>>().join(" · ").chars().take(160).collect();
            json!({"id":r["id"],"n":r["n"],"complete":r["complete"],"anchor":anchor,"preview":preview})
        }).collect();
        Item {
            id: "navigation".into(),
            namespace: "session".into(),
            kind: "navigation".into(),
            data: json!({"segment":self.segment(),"rounds":rounds}),
            fallback: Fallback {
                title: "轮导航".into(),
                text: format!("{} 轮", rounds.len()),
            },
        }
    }
    /// 只克隆本页正文；完整谱系仍是唯一的轮、段和原生位置依据。
    pub fn snapshot(&self) -> Vec<Item> {
        let page = self
            .page(&PageReq {
                limit: WINDOW as u32,
                ..Default::default()
            })
            .expect("default page");
        let mut items: Vec<_> = self
            .items
            .values()
            .filter(|i| control(i))
            .cloned()
            .collect();
        items.push(self.navigation());
        items.push(Item {
            id: "history".into(),
            namespace: "session".into(),
            kind: "history".into(),
            data: json!({"older":page.next}),
            fallback: Fallback {
                title: "历史".into(),
                text: String::new(),
            },
        });
        items.extend(page.items);
        // 进行中的条目必须在冷快照里保留完整累计内容，即使较新的条目超过一页。
        for item in self.items.values().filter(|i| {
            !control(i)
                && (i.data["complete"] == false
                    || (i.kind == "control" && i.data["state"] == "pending")
                    || (i.kind == "prompt"
                        && matches!(
                            i.data["state"].as_str(),
                            Some("held" | "waiting" | "pending" | "written" | "withdrawing")
                        )))
        }) {
            if !items.iter().any(|i| i.id == item.id) {
                items.push(item.clone());
            }
        }
        items
    }
    pub fn page(&self, request: &PageReq) -> Result<Page, String> {
        if request.limit == 0
            || request.limit > 100
            || [
                request.before.is_some(),
                request.after.is_some(),
                request.around.is_some(),
            ]
            .into_iter()
            .filter(|v| *v)
            .count()
                > 1
        {
            return Err("invalid_page".into());
        }
        let before = request
            .before
            .as_deref()
            .map(|s| self.decode(s))
            .transpose()?;
        let after = request
            .after
            .as_deref()
            .map(|s| self.decode(s))
            .transpose()?;
        let anchor = if let Some(id) = &request.around {
            let nav = self.navigation();
            let r = nav.data["rounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"].as_str() == Some(id))
                .ok_or("round_not_found")?;
            Some(
                r["anchor"]
                    .as_str()
                    .ok_or("history_unavailable")?
                    .to_owned(),
            )
        } else {
            None
        };
        let from = anchor.as_ref().and_then(|a| self.ids.get(a)).copied();
        let excluded: std::collections::BTreeSet<_> = self
            .get("lineage")
            .and_then(|i| i.data["inactive_messages"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut included = true;
        let body: Vec<_> = self
            .items
            .iter()
            .filter(|(_, i)| {
                if i.kind == "prompt" {
                    included = !i.data["message"]
                        .as_str()
                        .is_some_and(|id| excluded.contains(id));
                }
                !control(i) && (included || i.kind == "op" || i.kind == "asked")
            })
            .collect();
        let eligible: Vec<_> = body
            .iter()
            .copied()
            .filter(|(s, _)| {
                before.is_none_or(|b| **s < b)
                    && after.is_none_or(|a| **s > a)
                    && from.is_none_or(|a| **s >= a)
            })
            .collect();
        let range = if after.is_some() || from.is_some() {
            0..eligible.len().min(request.limit as usize)
        } else {
            eligible.len().saturating_sub(request.limit as usize)..eligible.len()
        };
        let selected = &eligible[range];
        Ok(Page {
            next: selected
                .first()
                .filter(|(s, _)| body.first().is_some_and(|(first, _)| first < s))
                .map(|(s, _)| self.cursor(**s)),
            newer: selected
                .last()
                .filter(|(s, _)| body.last().is_some_and(|(last, _)| last > s))
                .map(|(s, _)| self.cursor(**s)),
            items: selected.iter().map(|(_, i)| (*i).clone()).collect(),
            anchor,
            at: None,
        })
    }
}
