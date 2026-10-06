//! 谱系：段、承载区间及轮到原生位置的映射。纯值 fold；不访问后端、存储或时钟。
use nd_backend::{BackendSessionId, CarrierId, SessionId, Ticket};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePosition {
    pub carrier: CarrierId,
    pub backend_session: BackendSessionId,
    pub native: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub messages: Vec<String>,
    pub positions: Vec<NativePosition>,
    pub complete: bool,
    /// 最后一条 assistant 的原生记录位置，供已结束轮的分叉使用。
    pub last_assistant: Option<NativePosition>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub carrier: CarrierId,
    pub backend_session: BackendSessionId,
    /// 以整段的轮边界计数：[from, to)，None 表示仍承载后续轮。
    pub from: usize,
    pub to: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    pub turns: Vec<String>,
    pub bindings: Vec<Binding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Landing {
    message: String,
    ticket: Ticket,
    position: NativePosition,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    current: Option<String>,
    origin: Option<ForkOrigin>,
    segments: BTreeMap<String, Segment>,
    turns: BTreeMap<String, Turn>,
    landings: Vec<Landing>,
    observed: BTreeMap<String, String>,
    edges: Vec<Edge>,
    switches: Vec<BackendSwitch>,
    sync_points: BTreeMap<CarrierId, SyncPoint>,
    carriers: BTreeMap<CarrierId, KnownCarrier>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchKind {
    Rewind,
    Clear,
    ExternalContinuation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub through: Option<String>,
    pub kind: BranchKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyNode {
    pub segment: String,
    pub parent: Option<String>,
    pub shared_rounds: usize,
    pub current: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendSwitch {
    pub segment: String,
    pub from: CarrierId,
    pub to: CarrierId,
    pub after_rounds: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPoint {
    pub segment: String,
    pub through: Option<String>,
    pub invalid: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkOrigin {
    pub session: SessionId,
    pub segment: String,
    pub through: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownCarrier {
    pub segment: String,
    pub backend_session: BackendSessionId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentTarget {
    pub segment: String,
    pub carrier: CarrierId,
    pub backend_session: BackendSessionId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// 后端会话已存在（可仍是镜像或准备中的目标）；不改变当前承载区间。
    CarrierKnown {
        segment: String,
        carrier: CarrierId,
        backend_session: BackendSessionId,
    },
    Root {
        segment: String,
        carrier: CarrierId,
        backend_session: BackendSessionId,
    },
    Branch {
        segment: String,
        from: String,
        through: Option<String>,
        kind: BranchKind,
        carrier: CarrierId,
        backend_session: BackendSessionId,
    },
    Activate {
        segment: String,
    },
    SwitchBackend {
        segment: String,
        carrier: CarrierId,
        backend_session: BackendSessionId,
    },
    Imported {
        segment: String,
        carrier: CarrierId,
        through: String,
        positions: Vec<(String, NativePosition)>,
    },
    InvalidateSync {
        carrier: CarrierId,
        reason: String,
    },
    Landed {
        message: String,
        ticket: Ticket,
        position: NativePosition,
    },
    /// 适配器确认的实际落点；key 是这一原生回合的稳定身份，不是消息或票。
    TurnObserved {
        carrier: CarrierId,
        key: String,
        natives: Vec<String>,
        complete: bool,
        last_assistant: Option<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("谱系事实无效：{0}")]
pub struct Error(pub String);

impl Lineage {
    /// 返回新状态；失败不改变原状态，同一事实重放不重复开轮。
    pub fn fold(&self, event: &Event) -> Result<Self, Error> {
        let mut next = self.clone();
        next.apply(event)?;
        Ok(next)
    }
    pub fn origin(&self) -> Option<&ForkOrigin> {
        self.origin.as_ref()
    }
    /// 会话分叉只复制已结束前缀的谱系值，来源完全不变；不涉及 CLI 文件或任务。
    pub fn fork(
        &self,
        session: SessionId,
        from: &str,
        through: Option<String>,
        target: SegmentTarget,
    ) -> Result<Self, Error> {
        let n = self.prefix_len(from, &through)?;
        let source = self.segment(from)?;
        let mut child = Self::default().fold(&Event::Root {
            segment: target.segment.clone(),
            carrier: target.carrier,
            backend_session: target.backend_session,
        })?;
        child.origin = Some(ForkOrigin {
            session,
            segment: from.into(),
            through,
        });
        for id in &source.turns[..n] {
            child.turns.insert(id.clone(), self.turns[id].clone());
        }
        child.segments.get_mut(&target.segment).unwrap().turns = source.turns[..n].to_vec();
        Ok(child)
    }
    pub fn common_prefix_with(&self, a: &str, other: &Self, b: &str) -> Result<Vec<String>, Error> {
        Ok(self
            .segment(a)?
            .turns
            .iter()
            .zip(&other.segment(b)?.turns)
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.clone())
            .collect())
    }
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }
    pub fn segment(&self, id: &str) -> Result<&Segment, Error> {
        self.segments
            .get(id)
            .ok_or_else(|| Error(format!("未知段 {id}")))
    }
    pub fn turns(&self, segment: &str) -> Result<Vec<&Turn>, Error> {
        Ok(self
            .segment(segment)?
            .turns
            .iter()
            .map(|id| &self.turns[id])
            .collect())
    }
    pub fn carriers(&self) -> &BTreeMap<CarrierId, KnownCarrier> {
        &self.carriers
    }
    fn record_carrier(
        &mut self,
        segment: &str,
        carrier: &CarrierId,
        backend_session: &BackendSessionId,
    ) -> Result<(), Error> {
        let known = KnownCarrier {
            segment: segment.into(),
            backend_session: backend_session.clone(),
        };
        if self.carriers.get(carrier).is_some_and(|old| old != &known) {
            return Err(Error("承载位的段或后端会话身份冲突".into()));
        }
        self.carriers.insert(carrier.clone(), known);
        Ok(())
    }
    pub fn switches(&self) -> &[BackendSwitch] {
        &self.switches
    }
    pub fn sync_point(&self, carrier: &CarrierId) -> Option<&SyncPoint> {
        self.sync_points.get(carrier)
    }
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }
    pub fn common_prefix(&self, a: &str, b: &str) -> Result<Vec<String>, Error> {
        self.common_prefix_with(a, self, b)
    }
    /// 父节点先于子节点；兄弟按稳定段 id 排序，不靠消息文字猜亲缘。
    pub fn topology(&self) -> Vec<TopologyNode> {
        let mut out = vec![];
        let mut remaining: Vec<_> = self.segments.keys().cloned().collect();
        while !remaining.is_empty() {
            let before = remaining.len();
            remaining.retain(|id| {
                let edge = self.edges.iter().find(|e| &e.to == id);
                if edge.is_some_and(|e| !out.iter().any(|n: &TopologyNode| n.segment == e.from)) {
                    return true;
                }
                out.push(TopologyNode {
                    segment: id.clone(),
                    parent: edge.map(|e| e.from.clone()),
                    shared_rounds: edge
                        .map_or(0, |e| self.common_prefix(&e.from, id).unwrap().len()),
                    current: self.current.as_ref() == Some(id),
                });
                false
            });
            assert!(remaining.len() < before, "validated lineage is acyclic");
        }
        out
    }
    fn prefix_len(&self, from: &str, through: &Option<String>) -> Result<usize, Error> {
        let path = &self.segment(from)?.turns;
        let n = match through {
            None => 0,
            Some(id) => path
                .iter()
                .position(|t| t == id)
                .map(|i| i + 1)
                .ok_or_else(|| Error("截点不在来源段".into()))?,
        };
        if path[..n].iter().any(|id| !self.turns[id].complete) {
            return Err(Error("不能从进行中的轮分叉".into()));
        }
        Ok(n)
    }
    fn apply(&mut self, event: &Event) -> Result<(), Error> {
        match event {
            Event::CarrierKnown {
                segment,
                carrier,
                backend_session,
            } => {
                self.segment(segment)?;
                self.record_carrier(segment, carrier, backend_session)?;
            }
            Event::Root {
                segment,
                carrier,
                backend_session,
            } => {
                let value = Segment {
                    id: segment.clone(),
                    turns: vec![],
                    bindings: vec![Binding {
                        carrier: carrier.clone(),
                        backend_session: backend_session.clone(),
                        from: 0,
                        to: None,
                    }],
                };
                if let Some(old) = self.segments.get(segment) {
                    if old.bindings.first().is_some_and(|b| {
                        &b.carrier == carrier && &b.backend_session == backend_session
                    }) {
                        return Ok(());
                    }
                    return Err(Error("根段身份冲突".into()));
                }
                if self.current.is_some() {
                    return Err(Error("已有根段".into()));
                }
                self.record_carrier(segment, carrier, backend_session)?;
                self.segments.insert(segment.clone(), value);
                self.current = Some(segment.clone());
            }
            Event::Branch {
                segment,
                from,
                through,
                kind,
                carrier,
                backend_session,
            } => {
                if self.segments.contains_key(segment) {
                    if self.edges.iter().any(|e| {
                        &e.to == segment
                            && &e.from == from
                            && &e.through == through
                            && e.kind == *kind
                    }) && self
                        .segment(segment)?
                        .bindings
                        .iter()
                        .any(|b| &b.carrier == carrier && &b.backend_session == backend_session)
                    {
                        return Ok(());
                    }
                    return Err(Error("目标段已存在".into()));
                }
                if self
                    .segments
                    .values()
                    .any(|s| s.bindings.iter().any(|b| &b.carrier == carrier))
                {
                    return Err(Error("新段不能复用来源承载位".into()));
                }
                if *kind == BranchKind::Clear && through.is_some() {
                    return Err(Error("清空不能保留前缀".into()));
                }
                let count = self.prefix_len(from, through)?;
                let source = self.segment(from)?;
                let mut bindings: Vec<_> = source
                    .bindings
                    .iter()
                    .filter(|b| b.from < count)
                    .cloned()
                    .map(|mut b| {
                        b.to = Some(b.to.unwrap_or(count).min(count));
                        b
                    })
                    .collect();
                bindings.push(Binding {
                    carrier: carrier.clone(),
                    backend_session: backend_session.clone(),
                    from: count,
                    to: None,
                });
                let target = Segment {
                    id: segment.clone(),
                    turns: source.turns[..count].to_vec(),
                    bindings,
                };
                self.record_carrier(segment, carrier, backend_session)?;
                self.segments.insert(segment.clone(), target);
                self.edges.push(Edge {
                    from: from.clone(),
                    to: segment.clone(),
                    through: through.clone(),
                    kind: *kind,
                });
                if *kind != BranchKind::ExternalContinuation {
                    self.current = Some(segment.clone());
                }
            }
            Event::SwitchBackend {
                segment,
                carrier,
                backend_session,
            } => {
                let path = self.segment(segment)?;
                if path.turns.iter().any(|id| !self.turns[id].complete) {
                    return Err(Error("回合尚未结束".into()));
                }
                let after = path.turns.len();
                self.record_carrier(segment, carrier, backend_session)?;
                let path = self.segments.get_mut(segment).unwrap();
                let last = path.bindings.last_mut().unwrap();
                if &last.carrier == carrier && &last.backend_session == backend_session {
                    return Ok(());
                }
                last.to = Some(after);
                self.switches.push(BackendSwitch {
                    segment: segment.clone(),
                    from: last.carrier.clone(),
                    to: carrier.clone(),
                    after_rounds: after,
                });
                path.bindings.push(Binding {
                    carrier: carrier.clone(),
                    backend_session: backend_session.clone(),
                    from: after,
                    to: None,
                });
            }
            Event::Imported {
                segment,
                carrier,
                through,
                positions,
            } => {
                let n = self.prefix_len(segment, &Some(through.clone()))?;
                let path = self.segment(segment)?;
                let binding = self
                    .carriers
                    .get(carrier)
                    .filter(|b| &b.segment == segment)
                    .ok_or_else(|| Error("导入目标不在段内".into()))?;
                for (turn, pos) in positions {
                    if !path.turns[..n].contains(turn)
                        || &pos.carrier != carrier
                        || pos.backend_session != binding.backend_session
                    {
                        return Err(Error("导入映射超出前缀或目标".into()));
                    }
                }
                for (turn, pos) in positions {
                    if self
                        .turns
                        .iter()
                        .any(|(id, t)| id != turn && t.positions.contains(pos))
                    {
                        return Err(Error("原生位置不能对应两个不同的轮".into()));
                    }
                    let turn = self.turns.get_mut(turn).unwrap();
                    if !turn.positions.contains(pos) {
                        turn.positions.push(pos.clone());
                    }
                }
                self.sync_points.insert(
                    carrier.clone(),
                    SyncPoint {
                        segment: segment.clone(),
                        through: Some(through.clone()),
                        invalid: None,
                    },
                );
            }
            Event::InvalidateSync { carrier, reason } => {
                let known = self
                    .carriers
                    .get(carrier)
                    .ok_or_else(|| Error("同步点的承载位未知".into()))?;
                self.sync_points
                    .entry(carrier.clone())
                    .and_modify(|p| p.invalid = Some(reason.clone()))
                    .or_insert(SyncPoint {
                        segment: known.segment.clone(),
                        through: None,
                        invalid: Some(reason.clone()),
                    });
            }
            Event::Activate { segment } => {
                self.segment(segment)?;
                self.current = Some(segment.clone());
            }
            Event::Landed {
                message,
                ticket,
                position,
            } => {
                if !self.segments.values().any(|s| {
                    s.bindings.iter().any(|b| {
                        b.carrier == position.carrier
                            && b.backend_session == position.backend_session
                    })
                }) {
                    return Err(Error("落地位置没有对应的后端会话".into()));
                }
                if self.landings.iter().any(|l| {
                    l.position == *position && (&l.ticket != ticket || &l.message != message)
                }) {
                    return Err(Error("原生位置已属于另一条消息或票".into()));
                }
                let landing = Landing {
                    message: message.clone(),
                    ticket: ticket.clone(),
                    position: position.clone(),
                };
                if let Some(old) = self.landings.iter().find(|l| &l.ticket == ticket) {
                    if old != &landing {
                        return Err(Error("同票的落地位置冲突".into()));
                    }
                } else {
                    self.landings.push(landing);
                }
            }
            Event::TurnObserved {
                carrier,
                key,
                natives,
                complete,
                last_assistant,
            } => {
                let (segment, binding) = self
                    .segments
                    .values()
                    .find_map(|s| {
                        s.bindings
                            .last()
                            .filter(|b| &b.carrier == carrier)
                            .map(|b| (s.id.clone(), b.clone()))
                    })
                    .ok_or_else(|| Error(format!("未知承载位 {carrier}")))?;
                let landed: Vec<_> = natives
                    .iter()
                    .filter_map(|native| {
                        self.landings.iter().find(|l| {
                            l.position.carrier == *carrier
                                && l.position.backend_session == binding.backend_session
                                && &l.position.native == native
                        })
                    })
                    .cloned()
                    .collect();
                let identity = serde_json::to_string(&(carrier, key)).expect("string tuple");
                let mut known = self.observed.get(&identity).cloned();
                for id in &self.segment(&segment)?.turns {
                    if landed
                        .iter()
                        .any(|l| self.turns[id].positions.contains(&l.position))
                    {
                        if known.as_ref().is_some_and(|old| old != id) {
                            return Err(Error("一个落点不能合并两个已确认的轮".into()));
                        }
                        known = Some(id.clone());
                    }
                }
                if let Some(id) = &known
                    && self.turns[id].complete
                {
                    if landed
                        .iter()
                        .all(|l| self.turns[id].positions.contains(&l.position))
                    {
                        return Ok(());
                    }
                    return Err(Error("已结束的轮不能追加新的落地消息".into()));
                }
                // 工具结果、定时触发、任务通知没有新的人类提示，不计导航的轮。
                if landed.is_empty() && known.is_none() {
                    return Ok(());
                }
                let id = known.unwrap_or_else(|| {
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(identity.as_bytes());
                    format!(
                        "round-{}",
                        digest[..16]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    )
                });
                let turn = self.turns.entry(id.clone()).or_insert_with(|| Turn {
                    id: id.clone(),
                    messages: vec![],
                    positions: vec![],
                    complete: false,
                    last_assistant: None,
                });
                for l in landed {
                    if !turn.messages.contains(&l.message) {
                        turn.messages.push(l.message);
                    }
                    if !turn.positions.contains(&l.position) {
                        turn.positions.push(l.position);
                    }
                }
                turn.complete |= complete;
                if let Some(native) = last_assistant {
                    turn.last_assistant = Some(NativePosition {
                        carrier: carrier.clone(),
                        backend_session: binding.backend_session,
                        native: native.clone(),
                    });
                }
                let path = &mut self.segments.get_mut(&segment).unwrap().turns;
                if !path.contains(&id) {
                    path.push(id.clone());
                }
                self.observed.insert(identity, id);
            }
        }
        Ok(())
    }
}
