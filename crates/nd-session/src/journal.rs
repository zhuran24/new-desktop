//! 操作作者接口：`run(&View, &mut Journal)` 是纯函数，每个输入之后从头重跑；
//! 做过的步骤从操作账按键读回，没做过的作为「步」交给引擎，在同一事务里落库。
//!
//! 作者只经 Journal 发动作（`act`）、经独占登记放行（`claim`、`bind`）、等条件（`wait`）、
//! 落定（`settle`），取创建意图编号（`id`）。不读时钟、不取随机数、不做 I/O。
use crate::state::{Carrier, Core, Meta, Status};
use nd_backend::{Act, BackendSessionId, CarrierId, Done, Outcome, SessionId, Ticket};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;

/// 操作的只读视图。
pub struct View<'a> {
    pub(crate) core: &'a Core,
}
impl View<'_> {
    pub fn session(&self) -> &SessionId {
        &self.core.meta().id
    }
    pub fn meta(&self) -> &Meta {
        self.core.meta()
    }
    pub fn carrier(&self, id: &CarrierId) -> Option<&Carrier> {
        self.core.carriers.get(id)
    }
    pub fn current(&self) -> Option<&Carrier> {
        self.core.current_carrier()
    }
}

/// `run` 不往下走的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Halt {
    /// 让出：等某个结果或条件，本次事务照常提交。
    Yield,
    /// 做不下去：引擎收场（按登记倒序执行已激活的补偿）。
    Fail(String),
    /// 只能在第一个动作之前：命令被拒，什么都没做。
    Reject { code: String, now: Value },
    /// 至多一次的动作交付不明而作者要的是确定结果：停在未决，保留槽位与代持。
    Unresolved { key: String },
}

/// 落定的内容。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    /// 这个承载位成为会话的当前承载位。
    Current {
        carrier: CarrierId,
    },
    Status {
        status: Status,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum EntryBody {
    Act {
        carrier: CarrierId,
        act: Act,
        undo: Option<Act>,
        /// 当前尝试的票；None：还没放行（等独占）。
        ticket: Option<Ticket>,
        attempt: u32,
        waiting: Option<String>,
        outcome: Option<Outcome>,
    },
    Claim {
        decision: nd_claims::Admit,
    },
    Bind {
        conflict: Option<String>,
    },
    Wait {
        value: Value,
    },
    Settle {
        changes: Vec<Change>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub order: u64,
    pub body: EntryBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Running,
    /// 收场：先等在途动作有结果，再按登记倒序执行已激活的补偿（`comps` 是操作账的键）。
    Unwinding {
        reason: String,
        comps: Option<Vec<String>>,
        irreversible: Vec<String>,
    },
    Done,
    Compensated {
        reason: String,
    },
    Partial {
        reason: String,
        irreversible: Vec<String>,
    },
    Rejected {
        code: String,
        now: Value,
    },
    Unresolved {
        key: String,
    },
}
impl Phase {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Phase::Done
                | Phase::Compensated { .. }
                | Phase::Partial { .. }
                | Phase::Rejected { .. }
        )
    }
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Running => "running",
            Phase::Unwinding { .. } => "unwinding",
            Phase::Done => "done",
            Phase::Compensated { .. } => "compensated",
            Phase::Partial { .. } => "partial",
            Phase::Rejected { .. } => "rejected",
            Phase::Unresolved { .. } => "unresolved",
        }
    }
}

/// 一个操作的持久记录：操作账。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpRecord {
    pub id: String,
    pub spec: crate::ops::OpSpec,
    pub version: u32,
    pub phase: Phase,
    pub entries: BTreeMap<String, Entry>,
    pub next_order: u64,
    pub result: Option<Value>,
    /// 起它的命令（种子操作）或 None（引擎起的）。
    pub command: Option<String>,
}

/// `run` 交给引擎的新步。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Act {
        key: String,
        carrier: CarrierId,
        act: Act,
        undo: Option<Act>,
    },
    Claim {
        key: String,
        act: nd_claims::Act,
    },
    Bind {
        key: String,
        bs: BackendSessionId,
    },
    Wait {
        key: String,
        value: Value,
    },
    Settle {
        changes: Vec<Change>,
    },
}

pub struct Journal<'a> {
    pub(crate) op: &'a OpRecord,
    pub(crate) steps: Vec<Step>,
}
impl<'a> Journal<'a> {
    pub(crate) fn new(op: &'a OpRecord) -> Self {
        Self { op, steps: vec![] }
    }
    pub fn op_id(&self) -> &str {
        &self.op.id
    }
    /// 创建意图编号（承载位、后端进程编号、预定的后端会话 id）：由操作 id 和键确定性派生，
    /// 重跑、重启后不变。
    pub fn id(&self, key: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!("nd-id:{}:{key}", self.op.id).as_bytes());
        digest[..16].iter().map(|b| format!("{b:02x}")).collect()
    }
    /// 有副作用的事只经它；第一次求值时进操作账和发件箱，之后返回同一句柄。
    pub fn act(&mut self, key: &str, carrier: &CarrierId, act: Act) -> Handle<'_, 'a> {
        if let Some(Entry {
            body: EntryBody::Act { outcome, .. },
            ..
        }) = self.op.entries.get(key)
        {
            return Handle {
                outcome: outcome.clone(),
                fresh: None,
                key: key.to_owned(),
                journal: self,
            };
        }
        self.steps.push(Step::Act {
            key: key.to_owned(),
            carrier: carrier.clone(),
            act,
            undo: None,
        });
        let fresh = Some(self.steps.len() - 1);
        Handle {
            outcome: None,
            fresh,
            key: key.to_owned(),
            journal: self,
        }
    }
    /// 独占登记放行，同一事务：`Go` 返回凭据；`Wait` 让出，障碍消失后重跑再问；`No` 收场。
    pub fn claim(&mut self, key: &str, act: nd_claims::Act) -> Result<nd_claims::Pass, Halt> {
        match self.op.entries.get(key).map(|e| &e.body) {
            Some(EntryBody::Claim {
                decision: nd_claims::Admit::Go(pass),
            }) => Ok(pass.clone()),
            Some(EntryBody::Claim { decision }) => {
                Err(Halt::Fail(format!("独占登记不放行：{decision:?}")))
            }
            _ => {
                self.steps.push(Step::Claim {
                    key: key.to_owned(),
                    act,
                });
                Err(Halt::Yield)
            }
        }
    }
    /// 把后端会话绑到 `claim` 键那次放行的预留上；冲突时收场（双方已暂停写入）。
    pub fn bind(&mut self, claim: &str, bs: &BackendSessionId) -> Result<(), Halt> {
        let key = format!("bind:{claim}");
        match self.op.entries.get(&key).map(|e| &e.body) {
            Some(EntryBody::Bind { conflict: None }) => Ok(()),
            Some(EntryBody::Bind {
                conflict: Some(held_by),
            }) => Err(Halt::Fail(format!("后端会话已由 {held_by} 持有"))),
            _ => {
                self.steps.push(Step::Bind {
                    key: claim.to_owned(),
                    bs: bs.clone(),
                });
                Err(Halt::Yield)
            }
        }
    }
    /// 等条件成立；结论写进操作账，之后重跑读回同一个值。
    pub fn wait<T: Serialize + DeserializeOwned>(
        &mut self,
        key: &str,
        view: &View<'_>,
        probe: impl Fn(&View<'_>) -> Option<T>,
    ) -> Result<T, Halt> {
        if let Some(Entry {
            body: EntryBody::Wait { value },
            ..
        }) = self.op.entries.get(key)
        {
            return serde_json::from_value(value.clone())
                .map_err(|e| Halt::Fail(format!("操作账 {key} 读不回：{e}")));
        }
        let Some(found) = probe(view) else {
            return Err(Halt::Yield);
        };
        self.steps.push(Step::Wait {
            key: key.to_owned(),
            value: serde_json::to_value(&found).map_err(|e| Halt::Fail(e.to_string()))?,
        });
        Ok(found)
    }
    /// 落定：至多一次。
    pub fn settle(&mut self, changes: Vec<Change>) -> Result<(), Halt> {
        if self.op.entries.contains_key("settle") {
            return Ok(());
        }
        self.steps.push(Step::Settle { changes });
        Err(Halt::Yield)
    }
    pub fn fail(&self, why: impl Into<String>) -> Halt {
        Halt::Fail(why.into())
    }
    /// 只能在第一个动作之前。
    pub fn reject(&self, code: &str, now: Value) -> Halt {
        Halt::Reject {
            code: code.into(),
            now,
        }
    }
}

pub struct Handle<'j, 'a> {
    journal: &'j mut Journal<'a>,
    key: String,
    outcome: Option<Outcome>,
    fresh: Option<usize>,
}
impl Handle<'_, '_> {
    /// 补偿是数据，与正向动作同一事务落库；不接的就是不可逆。
    pub fn undo(self, compensation: Act) -> Self {
        if let Some(index) = self.fresh
            && let Some(Step::Act { undo, .. }) = self.journal.steps.get_mut(index)
        {
            *undo = Some(compensation);
        }
        self
    }
    /// 确定成功 → 完成值；被拒、明确失败 → 收场；交付不明 → 未决；还没结果 → 让出。
    pub fn ok(self) -> Result<Done, Halt> {
        match self.outcome {
            None => Err(Halt::Yield),
            Some(Outcome::Ok { done }) => Ok(done),
            Some(Outcome::Unknown { .. }) => Err(Halt::Unresolved { key: self.key }),
            Some(other) => Err(Halt::Fail(other.reason())),
        }
    }
    /// 作者自己处理失败和不明；只在还没结果时让出。
    pub fn outcome(self) -> Result<Outcome, Halt> {
        self.outcome.ok_or(Halt::Yield)
    }
}
