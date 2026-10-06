//! 界面只保存当前页和异步查询代次；正文与轮身份来自 nd-wire。
use nd_wire::{Page, Snapshot};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundView {
    pub id: String,
    pub n: u64,
    pub preview: String,
    pub anchor: Option<String>,
}
#[derive(Default)]
pub struct HistoryView {
    live: Option<Snapshot>,
    page: Option<Page>,
    rounds: Vec<RoundView>,
    generation: u64,
    pub loading: bool,
    pub error: Option<String>,
}
fn segment(s: &Snapshot) -> Option<&serde_json::Value> {
    s.items
        .iter()
        .find(|i| i.kind == "navigation")
        .map(|i| &i.data["segment"])
}
pub fn history_control(kind: &str) -> bool {
    matches!(
        kind,
        "header" | "draft" | "lineage" | "navigation" | "history"
    )
}
impl HistoryView {
    pub fn observe(&mut self, snapshot: Snapshot) {
        if self
            .live
            .as_ref()
            .is_some_and(|old| old.stream != snapshot.stream || segment(old) != segment(&snapshot))
        {
            self.latest();
        }
        self.rounds = snapshot
            .items
            .iter()
            .find(|i| i.kind == "navigation")
            .and_then(|i| i.data["rounds"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(RoundView {
                    id: r["id"].as_str()?.into(),
                    n: r["n"].as_u64()?,
                    preview: r["preview"].as_str().unwrap_or("历史尚不可用").into(),
                    anchor: r["anchor"].as_str().map(str::to_owned),
                })
            })
            .collect();
        if let Some(page) = &mut self.page {
            for item in &mut page.items {
                if let Some(new) = snapshot.items.iter().find(|new| new.id == item.id) {
                    *item = new.clone();
                }
            }
        }
        self.live = Some(snapshot);
    }
    pub fn rounds(&self) -> &[RoundView] {
        &self.rounds
    }
    pub fn request(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.generation
    }
    pub fn loaded(&mut self, generation: u64, result: Result<Page, String>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        match result {
            Ok(mut page) => {
                if let Some(live) = &self.live
                    && page
                        .at
                        .as_ref()
                        .is_some_and(|at| at.epoch == live.epoch && live.cursor > at.seq)
                {
                    for item in &mut page.items {
                        if let Some(current) = live.items.iter().find(|i| i.id == item.id) {
                            *item = current.clone();
                        }
                    }
                }
                self.page = Some(page);
            }
            Err(e) => self.error = Some(format!("历史读取失败：{e}")),
        }
        true
    }
    pub fn clear(&mut self) {
        self.latest();
        self.live = None;
        self.rounds.clear();
    }
    pub fn latest(&mut self) {
        self.generation += 1;
        self.loading = false;
        self.error = None;
        self.page = None;
    }
    pub fn is_latest(&self) -> bool {
        self.page.is_none()
    }
    pub fn older(&self) -> Option<String> {
        if let Some(page) = &self.page {
            page.next.clone()
        } else {
            self.live
                .as_ref()?
                .items
                .iter()
                .find(|i| i.kind == "history")?
                .data["older"]
                .as_str()
                .map(str::to_owned)
        }
    }
    pub fn newer(&self) -> Option<String> {
        self.page.as_ref()?.newer.clone()
    }
    pub fn anchor(&self) -> Option<&str> {
        self.page.as_ref()?.anchor.as_deref()
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        let mut snapshot = self.live.clone()?;
        if let Some(page) = &self.page {
            snapshot.items.retain(|i| history_control(&i.kind));
            snapshot.items.extend(page.items.clone());
        }
        Some(snapshot)
    }
}
