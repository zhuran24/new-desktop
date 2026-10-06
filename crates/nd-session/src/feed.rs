//! 一个会话流（`session/<id>`）的快照与事件：冷启动先给快照（带进行中条目的累积内容），
//! 同纪元的短断线按游标补事件。事件只在内存里；增量不进缓冲，游标越过增量就重给快照。
use crate::history::History;
use nd_wire::{Cursor, Event, Item, Response, Snapshot};
use std::collections::VecDeque;
use tokio::sync::broadcast;

const KEEP_EVENTS: usize = 256;

pub struct Feed {
    stream: String,
    epoch: String,
    cursor: u64,
    items: History,
    history: VecDeque<Event>,
    /// 最近一次只含增量的事件的游标；比它旧的游标补不齐，只能重给快照。
    last_live: u64,
    tx: broadcast::Sender<Event>,
}

/// 订阅的开头：快照，或同纪元补上的事件。
pub struct Start {
    pub first: Response,
    pub replay: Vec<Event>,
    pub events: broadcast::Receiver<Event>,
}

impl Feed {
    pub fn new(stream: String, epoch: String, items: Vec<(u64, Item)>) -> Self {
        let (tx, _) = broadcast::channel(1024);
        let mut history = History::new(stream.clone());
        for (_, item) in items {
            history.insert(item);
        }
        Self {
            stream,
            epoch,
            cursor: 0,
            items: history,
            history: VecDeque::new(),
            last_live: 0,
            tx,
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            stream: self.stream.clone(),
            epoch: self.epoch.clone(),
            cursor: self.cursor,
            items: self.items.snapshot(),
        }
    }
    pub fn page(&self, request: &nd_wire::PageReq) -> Result<nd_wire::Page, String> {
        let mut page = self.items.page(request)?;
        page.at = Some(Cursor {
            epoch: self.epoch.clone(),
            seq: self.cursor,
        });
        Ok(page)
    }
    /// 发一批变化；`live` 表示这批只有增量。
    pub fn publish(&mut self, upsert: Vec<(u64, Item)>, live: bool) {
        let upsert: Vec<(u64, Item)> = upsert
            .into_iter()
            .filter(|(_, item)| self.items.get(&item.id).is_none_or(|old| old != item))
            .collect();
        if upsert.is_empty() {
            return;
        }
        let before = self.items.snapshot();
        for (_, item) in &upsert {
            self.items.insert(item.clone());
        }
        let after = self.items.snapshot();
        let remove = before
            .iter()
            .filter(|old| !after.iter().any(|i| i.id == old.id))
            .map(|i| i.id.clone())
            .collect();
        let upsert = after.into_iter().filter(|i| !before.contains(i)).collect();
        self.cursor += 1;
        let event = Event {
            stream: self.stream.clone(),
            epoch: self.epoch.clone(),
            cursor: self.cursor,
            upsert,
            remove,
        };
        if live {
            self.last_live = self.cursor;
        } else {
            self.history.push_back(event.clone());
            if self.history.len() > KEEP_EVENTS {
                self.history.pop_front();
            }
        }
        let _ = self.tx.send(event);
    }
    pub fn subscribe(&self, since: Option<Cursor>) -> Start {
        let events = self.tx.subscribe();
        let earliest = self
            .history
            .front()
            .map_or(self.cursor, |event| event.cursor - 1);
        if let Some(since) = since.filter(|c| {
            c.epoch == self.epoch
                && c.seq >= earliest
                && c.seq >= self.last_live
                && c.seq <= self.cursor
        }) {
            let replay = self
                .history
                .iter()
                .filter(|event| event.cursor > since.seq)
                .cloned()
                .collect::<Vec<_>>();
            // 中间不能有缺口：环里的事件必须接上。
            let contiguous = replay
                .iter()
                .enumerate()
                .all(|(i, e)| e.cursor == since.seq + 1 + i as u64)
                && replay.last().map_or(since.seq, |e| e.cursor) == self.cursor;
            if contiguous {
                return Start {
                    first: Response::Resumed {
                        stream: self.stream.clone(),
                        epoch: self.epoch.clone(),
                        cursor: self.cursor,
                    },
                    replay,
                    events,
                };
            }
        }
        Start {
            first: Response::Snapshot {
                snapshot: self.snapshot(),
            },
            replay: vec![],
            events,
        }
    }
}
