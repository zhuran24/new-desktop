//! 会话执行器：每个装载中的会话一个，串行处理它的全部输入，唯一裁决它的一切改动。
//!
//! 一个输入一个事务：命令收据、批次的事实与检查点、发件箱、操作账、独占登记的放行、
//! 显示缓存同一事务提交；提交之后才发事件、给端口确认、把新票交给端口。纯增量不开事务。
mod drafts;
use drafts::RefOwner;
mod commands;
mod facts;
mod idle;
mod invocations;
mod sending;
mod view;
pub(crate) use view::header;
use view::{intent_name, invoke_shown};

use crate::{
    EngineConfig, Fault, Listing,
    feed::Feed,
    journal::{Change, Entry, EntryBody, Halt, Journal, OpRecord, Phase, Step, View},
    lineage::{Event as LineageEvent, NativePosition},
    ops::{Create, Launch, OpSpec, Reclaim},
    projection::{Projection, Shown},
    state::{self, Carrier, Core, Invoke, Issuer, Message, Meta, OutRow, Status},
};
use nd_backend::{
    Ack, Act, Admit, AdoptPart, BackendKind, Backends, Batch, CarrierRecord, CompactScope, Done,
    Drain, FactBody, Intent, Invocation, Invoked, Issued, Live, Outcome, PendingTicket, Refusal,
    SessionId, Ticket,
};
use nd_claims::Exclusivity;
use nd_store::{Store, Tx};
use nd_wire::{Command, CommandReply, Item, Receipt};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
use tokio::sync::{mpsc, oneshot};

pub(crate) enum Input {
    Command {
        command: Command,
        reply: oneshot::Sender<CommandReply>,
    },
    Batch(Batch),
    /// 不经批次回来的结果：端口同步拒绝。
    Settled {
        ticket: Ticket,
        outcome: Outcome,
    },
    /// 外部条件变了（独占登记的放行等）：重跑操作、重排发送台。
    Kick,
    Tick,
}

/// 执行器与名册、协议入口共享的部分。
pub(crate) struct Shared {
    pub feed: Mutex<Feed>,
    pub watchers: AtomicUsize,
    pub adopted: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct Deps {
    pub store: Arc<Store>,
    pub blobs: Arc<nd_store::Blobs>,
    pub claims: Arc<Exclusivity>,
    pub backends: Backends,
    pub config: EngineConfig,
    pub listing: Arc<Listing>,
}

/// 一次输入在事务里攒下、提交之后才做的事。
#[derive(Default)]
struct Effects {
    publish: Vec<(u64, Item)>,
    acks: Vec<(BackendKind, Ack)>,
    hand: Vec<Ticket>,
    /// 收据等动作有结果的命令：这个事务里落了收据，提交后回给还在等的连接。
    replies: Vec<(String, CommandReply)>,
}

/// 收据等动作有结果的命令（规格「收据时点」）：`!` 命令、总结、派 fork 型子代理。
pub(crate) const DELIVERY: [&str; 3] = ["session.shell", "session.compact", "session.subtask"];

enum Work {
    Command(Command),
    Batch(Batch),
    Settled(Ticket, Outcome),
    Kick,
    Tick,
}

pub(crate) enum Flow {
    Continue,
    Died,
}

pub(crate) struct Executor {
    id: SessionId,
    write_gen: u64,
    core: Core,
    born: bool,
    deps: Deps,
    projection: Projection,
    shared: Arc<Shared>,
    inbox: mpsc::Sender<Batch>,
    local: VecDeque<Input>,
    idle_since: Option<Instant>,
    /// 新建的会话在第一次提交之后才交端口 adopt（之前收件地址无处可交）。
    adopt_pending: bool,
    recovering: BTreeSet<nd_backend::CarrierId>,
    /// 等收据的连接：命令 id → 回应。只在内存里；断开的界面按命令 id 查收据。
    waiters: HashMap<String, Vec<oneshot::Sender<CommandReply>>>,
}

const CRASH: &str = "nd-session: simulated crash";
const FENCED: &str = "nd-session: fenced";

fn aborted(e: impl std::fmt::Display) -> nd_store::Error {
    nd_store::Error::Aborted(e.to_string())
}

fn rejected(code: &str, now: Value) -> Receipt {
    Receipt::Rejected {
        code: code.into(),
        now,
    }
}

impl Executor {
    /// 装载：写入代次加一（之后旧实例的提交得 Fenced），读回核心与显示缓存，交端口 adopt。
    pub fn open(
        id: SessionId,
        deps: Deps,
        shared: Arc<Shared>,
        inbox: mpsc::Sender<Batch>,
    ) -> nd_store::Result<Self> {
        let loaded = deps.store.write(|tx| {
            let Some(write_gen) = state::bump_gen(tx, &id)? else {
                return Ok(None);
            };
            Ok(state::load(tx, &id)?.map(|(_, core)| (write_gen, core)))
        })?;
        let (write_gen, mut core, born) = match loaded {
            Some((write_gen, core)) => (write_gen, core, true),
            None => (0, Core::default(), false),
        };
        let mut projection = Projection::restore(state::items(&deps.store, &id)?);
        if born {
            // 草稿是核心事实；即使旧版显示缓存没有它，冷启动也必须给出可编辑的版本。
            projection.apply(&Shown::Draft {
                draft: core.draft.clone(),
            });
        }
        // 恢复后的旧票全部交给 adopt 对账，不能把上次提交前的 handed=false 当作 Busy 重试。
        for row in core.outbox.values_mut() {
            row.handed = true;
        }
        let recovering = core
            .carriers
            .values()
            .filter(|c| c.run.is_some())
            .map(|c| c.id.clone())
            .chain(
                core.outbox
                    .values()
                    .chain(core.uncertain.values())
                    .map(|r| r.act.carrier().clone()),
            )
            .collect();
        let this = Self {
            id,
            write_gen,
            core,
            born,
            deps,
            projection,
            shared,
            inbox,
            local: VecDeque::new(),
            idle_since: None,
            adopt_pending: false,
            recovering,
            waiters: HashMap::new(),
        };
        if born {
            this.adopt();
        }
        Ok(this)
    }

    pub fn born(&self) -> bool {
        self.born
    }

    /// 本实例的流：纪元随实例换，快照从显示缓存起。
    pub fn install_feed(&mut self) {
        self.shared
            .adopted
            .store(self.recovering.is_empty(), Ordering::Release);
        if self.born {
            let mut data = header(&self.core);
            data["recovering"] = json!(self.recovering());
            self.projection.apply(&Shown::Header { data });
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        *self.shared.feed.lock().unwrap() = Feed::new(
            format!("session/{}", self.id),
            format!("{}-{nanos}", self.write_gen),
            self.projection.items_with_seq(),
        );
    }

    fn adopt(&self) {
        let carriers = self
            .core
            .carriers
            .values()
            .filter(|c| c.run.is_some())
            .map(|c| CarrierRecord {
                carrier: c.id.clone(),
                kind: c.kind.clone(),
                run: c.run.clone(),
                bs: Some(c.bs.clone()),
                adopt: c.adopt.clone(),
                checkpoint: c.checkpoint.clone(),
            })
            .collect();
        let pending = self
            .core
            .outbox
            .values()
            .chain(self.core.uncertain.values())
            .filter(|r| r.outcome.is_none() || matches!(r.outcome, Some(Outcome::Unknown { .. })))
            .map(|r| PendingTicket {
                unknown: matches!(r.outcome, Some(Outcome::Unknown { .. })),
                issued: r.issued.clone(),
                act: r.act.clone(),
            })
            .collect();
        let kinds: BTreeMap<_, _> = self
            .core
            .carriers
            .values()
            .map(|c| (c.id.clone(), c.kind.clone()))
            .collect();
        self.deps.backends.adopt(
            AdoptPart {
                session: self.id.clone(),
                inbox: self.inbox.clone(),
                carriers,
                pending,
            },
            |c| kinds.get(c).cloned(),
        );
    }

    fn recovering(&self) -> bool {
        !self.recovering.is_empty()
            || self.deps.claims.recovery().ok() != Some(nd_claims::Recovery::Ready)
    }

    fn fault(&self, point: Fault) -> bool {
        self.deps
            .config
            .faults
            .as_ref()
            .is_some_and(|f| f.crash(&self.id, point))
    }

    pub fn next_local(&mut self) -> Option<Input> {
        self.local.pop_front()
    }

    /// 处理一个输入。`Died` 表示（测试构建里）模拟的崩溃：执行器就此停下，内存状态丢掉。
    pub fn handle(&mut self, input: Input) -> Flow {
        self.clear_busy_idle_clock();
        if let Input::Batch(batch) = &input
            && batch.facts.is_empty()
            && batch.checkpoint.is_none()
        {
            // 纯增量不开事务（C3）。
            self.publish_live(batch.live.clone());
            return Flow::Continue;
        }
        if let Input::Tick = input
            && !self.core.outbox.values().any(|r| !r.handed)
            && !self.idle_due()
        {
            return Flow::Continue;
        }
        // 外部条件变了，但没有在等它的操作或消息：不开事务。
        if let Input::Kick = input
            && self.recovering.is_empty()
            && self.core.ops.is_empty()
            && self
                .core
                .messages
                .values()
                .all(|m| m.queue.ticket.is_some())
            && self.core.invokes.values().all(|i| i.queue.ticket.is_some())
            && self
                .projection
                .items()
                .iter()
                .any(|i| i.id == "header" && i.data["recovering"] == false)
        {
            return Flow::Continue;
        }
        let pending_digest = match &input {
            Input::Command { command, .. } => {
                self.core.invokes.get(&command.id).map(|p| p.digest.clone())
            }
            _ => None,
        };
        let input = match (pending_digest, input) {
            (Some(digest), Input::Command { command, reply }) => {
                // 收据还没落：同内容的重试等同一个结果，不同内容回 conflict。
                if digest == command.content_hash() {
                    self.waiters.entry(command.id).or_default().push(reply);
                } else {
                    let _ = reply.send(CommandReply::Conflict);
                }
                return Flow::Continue;
            }
            (_, input) => input,
        };
        let deferred = match &input {
            Input::Command { command, .. } => Some(command.id.clone()),
            _ => None,
        };
        let (reply, work) = match input {
            Input::Command { command, reply } => (Some(reply), Work::Command(command)),
            Input::Batch(batch) => (None, Work::Batch(batch)),
            Input::Settled { ticket, outcome } => (None, Work::Settled(ticket, outcome)),
            Input::Kick => (None, Work::Kick),
            Input::Tick => (None, Work::Tick),
        };
        let mut fx = Effects::default();
        let mut live = vec![];
        let store = self.deps.store.clone();
        let recovering_before = self.recovering.clone();
        let result = store.write(|tx| {
            if self.born && state::write_gen(tx, &self.id)? != Some(self.write_gen) {
                return Err(aborted(FENCED));
            }
            let value = self.fold(tx, work, &mut fx, &mut live)?;
            if self.born {
                self.settle_all(tx, &mut fx)?;
                let retry: Vec<_> = self
                    .core
                    .outbox
                    .iter()
                    .filter(|(t, r)| !r.handed && !fx.hand.contains(t))
                    .map(|(t, _)| t.clone())
                    .collect();
                fx.hand.extend(retry);
                self.persist(tx, &mut fx)?;
            }
            if self.fault(Fault::BeforeCommit) {
                return Err(aborted(CRASH));
            }
            Ok(value)
        });
        let value = match result {
            Ok(value) => value,
            Err(nd_store::Error::Aborted(why)) if why == CRASH => return Flow::Died,
            Err(e) => {
                // 事务回滚：内存状态按库里的重新读回，不留半截改动。
                self.recovering = recovering_before;
                self.reload();
                if let Some(reply) = reply {
                    let _ = reply.send(CommandReply::Unavailable {
                        code: None,
                        reason: e.to_string(),
                    });
                }
                return Flow::Continue;
            }
        };
        if self.fault(Fault::AfterCommit) {
            return Flow::Died;
        }
        // 受理了、收据等动作有结果：先挂着。结果可能就在这个事务里定了（例如当场被拒），
        // 所以挂在交出回应之前，结果入账的事务提交后统一回。
        let mut reply = reply;
        if value.is_none()
            && let Some(id) = deferred.filter(|id| {
                self.core.invokes.contains_key(id) || fx.replies.iter().any(|(r, _)| r == id)
            })
            && let Some(reply) = reply.take()
        {
            self.waiters.entry(id).or_default().push(reply);
        }
        // 提交之后：先追加事件、交出发件，命令的回应最后给（C1）。
        self.after_commit(fx, live);
        if self.fault(Fault::AfterHandoff) {
            return Flow::Died;
        }
        if let Some(reply) = reply {
            let _ = reply.send(value.unwrap_or(CommandReply::Unavailable {
                code: None,
                reason: "no reply".into(),
            }));
        }
        Flow::Continue
    }

    fn reload(&mut self) {
        match self.deps.store.write(|tx| state::load(tx, &self.id)) {
            Ok(Some((_, core))) => self.core = core,
            Ok(None) => {
                self.core = Core::default();
                self.born = false;
            }
            Err(_) => {}
        }
        if let Ok(items) = state::items(&self.deps.store, &self.id) {
            self.projection = Projection::restore(items);
        }
    }

    fn publish_live(&mut self, live: Vec<Live>) {
        let mut changed = vec![];
        for Live::Delta { item, kind, text } in live {
            if let Some(c) = self.projection.apply(&Shown::Delta { item, kind, text }) {
                changed.push(c);
            }
        }
        self.shared.feed.lock().unwrap().publish(changed, true);
    }

    fn after_commit(&mut self, mut fx: Effects, live: Vec<Live>) {
        self.clear_busy_idle_clock();
        fx.hand.sort_by_key(|ticket| {
            self.core
                .outbox
                .get(ticket)
                .map(|row| match &row.issuer {
                    Issuer::Message { id, .. } => {
                        (1, self.core.messages.get(id).map_or(0, |m| m.queue.arrival))
                    }
                    Issuer::Invoke { id } => {
                        (1, self.core.invokes.get(id).map_or(0, |i| i.queue.arrival))
                    }
                    _ => (0, 0),
                })
                .unwrap_or((0, 0))
        });
        self.shared
            .adopted
            .store(self.recovering.is_empty(), Ordering::Release);
        if std::mem::take(&mut self.adopt_pending) {
            // 新会话：只登记收件地址；这个事务里签发的票随后照常交出，不算恢复对账。
            self.deps.backends.adopt(
                AdoptPart {
                    session: self.id.clone(),
                    inbox: self.inbox.clone(),
                    carriers: vec![],
                    pending: vec![],
                },
                |_| None,
            );
        }
        self.shared.feed.lock().unwrap().publish(fx.publish, false);
        self.publish_live(live);
        for (id, reply) in fx.replies {
            for waiter in self.waiters.remove(&id).unwrap_or_default() {
                let _ = waiter.send(reply.clone());
            }
        }
        self.deps.listing.update(&self.core);
        for (kind, ack) in fx.acks {
            self.deps.backends.committed(&kind, ack);
        }
        for ticket in fx.hand {
            let Some(row) = self.core.outbox.get_mut(&ticket) else {
                continue;
            };
            row.handed = true;
            let admit = self
                .deps
                .backends
                .act(&row.kind, row.issued.clone(), row.act.clone());
            if let Admit::Rejected { reject } = admit {
                if reject == nd_backend::Reject::Busy {
                    row.handed = false;
                    continue;
                }
                self.local.push_back(Input::Settled {
                    ticket,
                    outcome: Outcome::Rejected { reject },
                });
            }
        }
    }

    fn show(&mut self, tx: &Tx<'_>, fx: &mut Effects, shown: Shown) -> nd_store::Result<()> {
        if let Some((seq, item)) = self.projection.apply(&shown) {
            state::put_item(tx, &self.id, &item, seq)?;
            fx.publish.push((seq, item));
        }
        Ok(())
    }

    // —— 输入 ——

    fn fold(
        &mut self,
        tx: &mut Tx<'_>,
        work: Work,
        fx: &mut Effects,
        live: &mut Vec<Live>,
    ) -> nd_store::Result<Option<CommandReply>> {
        match work {
            Work::Command(command) if DELIVERY.contains(&command.name.as_str()) => {
                let keep = self.deps.config.receipt_keep_ms;
                let receipt = match nd_ledger::begin(tx, &command)? {
                    nd_ledger::Begin::Prior(reply) => return Ok(Some(reply)),
                    nd_ledger::Begin::Invalid => nd_ledger::invalid(),
                    nd_ledger::Begin::New => {
                        if self.recovering() {
                            return Err(aborted("守护进程恢复中，命令未受理"));
                        }
                        match self.invoke_command(tx, &command, fx)? {
                            Some(receipt) => receipt,
                            // 受理了：意图已在核心里，收据等结果。
                            None => return Ok(None),
                        }
                    }
                };
                nd_ledger::record(tx, &command.id, &command.content_hash(), &receipt, keep)?;
                Ok(Some(CommandReply::Receipt { receipt }))
            }
            Work::Command(command) => {
                let keep = self.deps.config.receipt_keep_ms;
                let reply = nd_ledger::execute(tx, &command, keep, |tx| {
                    let control = matches!(
                        command.name.as_str(),
                        "session.interrupt" | "session.withdraw"
                    );
                    let recovering = if control {
                        // 当前实现每个会话的控制来源先追平；不等待其他会话或独占登记的写入闸门。
                        !self.recovering.is_empty()
                    } else {
                        self.recovering()
                    };
                    if recovering {
                        return Err(aborted("守护进程恢复中，命令未受理"));
                    }
                    self.command(tx, &command, fx)
                })?;
                Ok(Some(reply))
            }
            Work::Batch(batch) => {
                self.batch(tx, batch, fx, live)?;
                Ok(None)
            }
            Work::Settled(ticket, outcome) => {
                self.record_outcome(tx, fx, &ticket, outcome)?;
                Ok(None)
            }
            Work::Kick => Ok(None),
            Work::Tick => {
                if !self.recovering()
                    && self.idle_due()
                    && self.core.ops.is_empty()
                    && let Some(carrier) = self.core.current.clone()
                {
                    self.start_op(tx, fx, OpSpec::Reclaim(Reclaim { carrier }), None)?;
                }
                Ok(None)
            }
        }
    }

    // —— 操作 ——

    fn start_op(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        spec: OpSpec,
        command: Option<String>,
    ) -> nd_store::Result<String> {
        self.core.next_op += 1;
        let id = format!("{}:{}:{}", self.id, spec.kind(), self.core.next_op);
        let record = OpRecord {
            id: id.clone(),
            version: spec.version(),
            spec,
            phase: Phase::Running,
            entries: BTreeMap::new(),
            next_order: 0,
            result: None,
            command,
        };
        self.show_op(tx, fx, &record)?;
        self.core.ops.insert(id.clone(), record);
        Ok(id)
    }

    fn show_op(&mut self, tx: &Tx<'_>, fx: &mut Effects, op: &OpRecord) -> nd_store::Result<()> {
        let (reason, irreversible) = match &op.phase {
            Phase::Unwinding {
                reason,
                irreversible,
                ..
            } => (Some(reason.clone()), irreversible.clone()),
            Phase::Compensated { reason } => (Some(reason.clone()), vec![]),
            Phase::Partial {
                reason,
                irreversible,
            } => (Some(reason.clone()), irreversible.clone()),
            Phase::Rejected { code, .. } => (Some(code.clone()), vec![]),
            Phase::Unresolved { key } => (Some(format!("{key} 交付不明")), vec![]),
            _ => (
                op.entries.values().find_map(|e| match &e.body {
                    EntryBody::Act {
                        waiting: Some(w), ..
                    } => Some(format!("等独占：{w}")),
                    _ => None,
                }),
                vec![],
            ),
        };
        self.show(
            tx,
            fx,
            Shown::Op {
                id: op.id.clone(),
                kind: op.spec.kind().into(),
                phase: op.phase.name().into(),
                reason,
                irreversible,
            },
        )
    }

    /// 引擎保证 G2：每个开事务的输入之后重跑所有进行中的操作，直到没有新的步；
    /// 发送台随之重排（新票、代持、按需拉起）。
    fn settle_all(&mut self, tx: &mut Tx<'_>, fx: &mut Effects) -> nd_store::Result<()> {
        if self.recovering() {
            return Ok(());
        }
        for _ in 0..64 {
            if self.deps.config.auto_title
                && self.core.meta().status == Status::Active
                && self.core.messages.is_empty()
                && self.core.meta().title.may_auto_generate()
                && self
                    .core
                    .current_carrier()
                    .is_some_and(|c| c.alive && !c.turn_running)
                && !self.core.ops.values().any(|op| op.spec.structural())
            {
                let title = self.core.meta().title.seed.clone().unwrap();
                let carrier = self.core.current.clone().unwrap();
                self.core.meta.as_mut().unwrap().title.attempted = true;
                self.start_op(
                    tx,
                    fx,
                    OpSpec::Title(crate::ops::Title {
                        carrier,
                        request: crate::ops::TitleRequest::Generate { description: title },
                    }),
                    None,
                )?;
            }
            let mut progress = false;
            let mut ids: Vec<String> = self.core.ops.keys().cloned().collect();
            ids.sort_by_key(|id| id.rsplit(':').next().and_then(|n| n.parse::<u64>().ok()));
            for id in ids {
                progress |= self.advance(tx, fx, &id)?;
            }
            progress |= self.pump(tx, fx)?;
            if !progress {
                return Ok(());
            }
        }
        Err(aborted("操作重跑不收敛"))
    }

    fn advance(&mut self, tx: &mut Tx<'_>, fx: &mut Effects, id: &str) -> nd_store::Result<bool> {
        let Some(mut op) = self.core.ops.remove(id) else {
            return Ok(false);
        };
        let before = op.clone();
        let result = self.advance_op(tx, fx, &mut op);
        let changed = op != before;
        if changed {
            self.show_op(tx, fx, &op)?;
        }
        if op.phase.terminal() {
            self.finish_op(tx, fx, &op)?;
        } else {
            self.core.ops.insert(id.to_owned(), op);
        }
        result.map(|progress| progress || changed)
    }

    fn advance_op(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        op: &mut OpRecord,
    ) -> nd_store::Result<bool> {
        if op.version != op.spec.version() && matches!(op.phase, Phase::Running) {
            // G9：代码换了版本，不重跑，按存下的补偿收场。
            op.phase = Phase::Unwinding {
                reason: "操作版本对不上".into(),
                comps: None,
                irreversible: vec![],
            };
        }
        let mut progress = false;
        // 等独占的动作：障碍可能已经消失，按当下事实再问一次。
        let waiting: Vec<String> = op
            .entries
            .iter()
            .filter(|(_, e)| {
                matches!(
                    e.body,
                    EntryBody::Act {
                        ticket: None,
                        outcome: None,
                        ..
                    }
                )
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in waiting {
            progress |= self.issue_op_act(tx, fx, op, &key)?;
        }
        loop {
            match op.phase.clone() {
                Phase::Running => {
                    let (result, steps) = self.run_once(op);
                    if self.deps.config.check_purity {
                        let again = self.run_once(op);
                        assert_eq!(
                            (&result, &steps),
                            (&again.0, &again.1),
                            "操作 {} 的 run 不是纯函数",
                            op.id
                        );
                    }
                    if !steps.is_empty() {
                        if self.apply_steps(tx, fx, op, steps)? {
                            progress = true;
                            continue;
                        }
                        return Ok(progress);
                    }
                    match result {
                        Ok(value) => {
                            op.result = Some(value);
                            op.phase = Phase::Done;
                            return Ok(true);
                        }
                        Err(Halt::Yield) => return Ok(progress),
                        Err(Halt::Fail(reason)) => {
                            op.phase = Phase::Unwinding {
                                reason,
                                comps: None,
                                irreversible: vec![],
                            };
                            progress = true;
                        }
                        Err(Halt::Reject { code, now }) => {
                            op.phase = Phase::Rejected { code, now };
                            return Ok(true);
                        }
                        Err(Halt::Unresolved { key }) => {
                            op.phase = Phase::Unresolved { key };
                            return Ok(true);
                        }
                    }
                }
                Phase::Unwinding { .. } => return Ok(self.unwind(tx, fx, op)? || progress),
                _ => return Ok(progress),
            }
        }
    }

    fn run_once(&self, op: &OpRecord) -> (Result<Value, Halt>, Vec<Step>) {
        let view = View { core: &self.core };
        let mut journal = Journal::new(op);
        let result = op.spec.run(&view, &mut journal);
        (result, journal.steps)
    }

    /// 把新步落进操作账。返回是否记下了什么；`claim` 等到 `Wait` 时什么也不记，操作让出。
    fn apply_steps(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        op: &mut OpRecord,
        steps: Vec<Step>,
    ) -> nd_store::Result<bool> {
        let mut recorded = false;
        for step in steps {
            let order = op.next_order;
            match step {
                Step::Act {
                    key,
                    carrier,
                    act,
                    undo,
                } => {
                    op.next_order += 1;
                    op.entries.insert(
                        key.clone(),
                        Entry {
                            order,
                            body: EntryBody::Act {
                                carrier,
                                act,
                                undo,
                                ticket: None,
                                attempt: 0,
                                waiting: None,
                                outcome: None,
                            },
                        },
                    );
                    self.issue_op_act(tx, fx, op, &key)?;
                    recorded = true;
                }
                Step::Claim { key, act } => {
                    let cause = format!("{}/{key}", op.id);
                    let decision = self.deps.claims.admit(tx, &cause, &act)?;
                    if matches!(decision, nd_claims::Admit::Wait(_)) {
                        return Ok(recorded);
                    }
                    op.next_order += 1;
                    op.entries.insert(
                        key,
                        Entry {
                            order,
                            body: EntryBody::Claim { decision },
                        },
                    );
                    recorded = true;
                }
                Step::Bind { key, bs } => {
                    let cause = format!("{}/{key}", op.id);
                    let conflict = match self.deps.claims.bind(tx, &cause, &bs)? {
                        nd_claims::BindResult::Bound => None,
                        nd_claims::BindResult::Conflict { held_by } => Some(held_by),
                    };
                    op.next_order += 1;
                    op.entries.insert(
                        format!("bind:{key}"),
                        Entry {
                            order,
                            body: EntryBody::Bind { conflict },
                        },
                    );
                    recorded = true;
                }
                Step::Wait { key, value } => {
                    op.next_order += 1;
                    op.entries.insert(
                        key,
                        Entry {
                            order,
                            body: EntryBody::Wait { value },
                        },
                    );
                    recorded = true;
                }
                Step::Settle { changes } => {
                    for change in &changes {
                        match change {
                            Change::Current { carrier } => {
                                self.core.current = Some(carrier.clone())
                            }
                            Change::Status { status } => {
                                if let Some(meta) = &mut self.core.meta {
                                    meta.status = *status;
                                }
                            }
                        }
                    }
                    op.next_order += 1;
                    op.entries.insert(
                        "settle".into(),
                        Entry {
                            order,
                            body: EntryBody::Settle { changes },
                        },
                    );
                    recorded = true;
                }
            }
        }
        Ok(recorded)
    }

    /// 给操作账里的一个动作签发票。写类动作（发送）先经独占登记放行（G12）。
    fn issue_op_act(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        op: &mut OpRecord,
        key: &str,
    ) -> nd_store::Result<bool> {
        let Some(Entry {
            body:
                EntryBody::Act {
                    carrier,
                    act,
                    ticket,
                    attempt,
                    waiting,
                    outcome,
                    ..
                },
            ..
        }) = op.entries.get_mut(key)
        else {
            return Ok(false);
        };
        if ticket.is_some() || outcome.is_some() {
            return Ok(false);
        }
        let issued = Ticket(format!("{}/{key}#{attempt}", op.id));
        if let Act::Open { spec, .. } = &*act {
            let bs = spec.origin.backend_session().clone();
            self.core
                .carriers
                .entry(carrier.clone())
                .or_insert_with(|| Carrier {
                    id: carrier.clone(),
                    kind: spec.profile.kind.clone(),
                    bs,
                    run: None,
                    alive: false,
                    readiness: None,
                    interaction: Default::default(),
                    adopt: Value::Null,
                    checkpoint: None,
                    drain: Drain::Unknown {
                        why: "还没有后台任务表".into(),
                    },
                    turn_running: false,
                    turns: 0,
                    features: vec![],
                });
        }
        let Some(kind) = self.core.carriers.get(carrier).map(|c| c.kind.clone()) else {
            *outcome = Some(Outcome::Rejected {
                reject: nd_backend::Reject::NoCarrier,
            });
            return Ok(true);
        };
        if matches!(&*act, Act::Send { .. }) {
            let bs = self.core.carriers[carrier].bs.clone();
            match self.deps.claims.admit(
                tx,
                &issued.0,
                &nd_claims::Act::Write {
                    session: self.id.0.clone(),
                    bs,
                },
            )? {
                nd_claims::Admit::Go(_) => {}
                nd_claims::Admit::Wait(obstacle) => {
                    let why = format!("{obstacle:?}");
                    let changed = waiting.as_deref() != Some(why.as_str());
                    *waiting = Some(why);
                    return Ok(changed);
                }
                nd_claims::Admit::No(refusal) => {
                    *outcome = Some(Outcome::Refused {
                        refusal: Refusal::Other {
                            why: format!("独占登记不放行：{refusal:?}"),
                        },
                    });
                    return Ok(true);
                }
            }
        }
        *waiting = None;
        *ticket = Some(issued.clone());
        let display = matches!(act, Act::Send { .. }).then(|| format!("{}:{key}", op.id));
        let row = OutRow {
            issued: Issued {
                ticket: issued.clone(),
                session: self.id.clone(),
                write_gen: self.write_gen,
            },
            act: act.clone(),
            kind,
            issuer: Issuer::Op {
                op: op.id.clone(),
                key: key.to_owned(),
            },
            display: display.clone(),
            outcome: None,
            handed: false,
        };
        if let (Some(display), Act::Send { msg, .. }) = (display, &row.act) {
            let shown = Shown::Prompt {
                id: display,
                text: msg.text.clone(),
                attachments: msg.attachments.clone(),
                intent: intent_name(msg.intent).into(),
                state: "pending".into(),
                native: None,
                reason: None,
            };
            self.show(tx, fx, shown)?;
        }
        self.core.outbox.insert(issued.clone(), row);
        fx.hand.push(issued);
        Ok(true)
    }

    /// 收场三步：等在途的正向动作有结果 → 按登记倒序执行已激活的补偿 → 终态。
    /// 做过（或可能做过）不可逆动作的终态是 Partial，否则 Compensated。
    fn unwind(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        op: &mut OpRecord,
    ) -> nd_store::Result<bool> {
        let Phase::Unwinding {
            reason,
            comps,
            irreversible,
        } = op.phase.clone()
        else {
            return Ok(false);
        };
        let mut progress = false;
        let comps = match comps {
            Some(comps) => comps,
            None => {
                let forward = |k: &String| !k.starts_with("undo:");
                if op.entries.iter().any(|(k, e)| {
                    forward(k)
                        && matches!(
                            e.body,
                            EntryBody::Act {
                                ticket: Some(_),
                                outcome: None,
                                ..
                            }
                        )
                }) {
                    return Ok(false);
                }
                // 还没放行的动作明确没做：就此作废。
                for (k, e) in op.entries.iter_mut() {
                    if forward(k)
                        && let EntryBody::Act {
                            ticket: None,
                            outcome: slot @ None,
                            ..
                        } = &mut e.body
                    {
                        *slot = Some(Outcome::Refused {
                            refusal: Refusal::Other {
                                why: "收场时还没放行，作废".into(),
                            },
                        });
                    }
                }
                let mut acts: Vec<(&String, &Entry)> =
                    op.entries.iter().filter(|(k, _)| forward(k)).collect();
                acts.sort_by_key(|(_, e)| std::cmp::Reverse(e.order));
                let mut comps = vec![];
                let mut irreversible = irreversible.clone();
                for (key, entry) in acts {
                    if let EntryBody::Act {
                        undo,
                        outcome: Some(outcome),
                        ..
                    } = &entry.body
                        && outcome.possibly_applied()
                    {
                        match undo {
                            Some(_) => comps.push(key.clone()),
                            None => irreversible.push(match outcome {
                                Outcome::Ok { .. } => key.clone(),
                                _ => format!("{key}（可能已做）"),
                            }),
                        }
                    }
                }
                op.phase = Phase::Unwinding {
                    reason: reason.clone(),
                    comps: Some(comps.clone()),
                    irreversible: irreversible.clone(),
                };
                progress = true;
                comps
            }
        };
        let Phase::Unwinding { irreversible, .. } = op.phase.clone() else {
            unreachable!()
        };
        for key in &comps {
            let undo_key = format!("undo:{key}");
            match op.entries.get(&undo_key).map(|e| &e.body) {
                None => {
                    let Some(EntryBody::Act {
                        carrier,
                        undo: Some(undo),
                        ..
                    }) = op.entries.get(key).map(|e| e.body.clone())
                    else {
                        continue;
                    };
                    let order = op.next_order;
                    op.next_order += 1;
                    op.entries.insert(
                        undo_key.clone(),
                        Entry {
                            order,
                            body: EntryBody::Act {
                                carrier,
                                act: undo,
                                undo: None,
                                ticket: None,
                                attempt: 0,
                                waiting: None,
                                outcome: None,
                            },
                        },
                    );
                    self.issue_op_act(tx, fx, op, &undo_key)?;
                    return Ok(true);
                }
                Some(EntryBody::Act { outcome: None, .. }) => return Ok(progress),
                Some(_) => {}
            }
        }
        op.phase = if irreversible.is_empty() {
            Phase::Compensated { reason }
        } else {
            Phase::Partial {
                reason,
                irreversible,
            }
        };
        Ok(true)
    }

    /// 操作到终态之后：种子操作决定会话撤掉还是部分完成；按需拉起失败时代持的消息报失败。
    fn finish_op(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        op: &OpRecord,
    ) -> nd_store::Result<()> {
        let failure = match &op.phase {
            Phase::Done => None,
            Phase::Compensated { reason } => Some(reason.clone()),
            Phase::Partial { reason, .. } => Some(reason.clone()),
            Phase::Rejected { code, .. } => Some(code.clone()),
            _ => return Ok(()),
        };
        match (&op.spec, &op.phase) {
            (
                OpSpec::Create(create),
                Phase::Compensated { reason } | Phase::Rejected { code: reason, .. },
            ) => {
                // 首条提示还没进入发送步骤时没有历史条目引用附件；撤掉种子一并释放。
                // 已有提示的失败/交付不明仍保留附件，供用户核对和另发。
                if !op.entries.contains_key("first") {
                    self.reference_attachments(
                        tx,
                        RefOwner::Message {
                            session: &self.id,
                            command: &self.core.meta().created_by,
                        },
                        &create.attachments,
                        false,
                    )?;
                }
                if let Some(meta) = &mut self.core.meta {
                    meta.status = Status::Withdrawn;
                    meta.note = Some(reason.clone());
                }
                self.fail_messages(tx, fx, "会话创建失败，已撤掉")?;
            }
            (
                OpSpec::Create(_),
                Phase::Partial {
                    reason,
                    irreversible,
                },
            ) => {
                if let Some(meta) = &mut self.core.meta {
                    meta.status = Status::Partial;
                    meta.note = Some(reason.clone());
                    meta.irreversible = irreversible.clone();
                }
                if self.core.current_carrier().is_none() {
                    self.fail_messages(tx, fx, "会话创建部分完成，没有可接收输入的当前承载位")?;
                }
            }
            (OpSpec::Launch(_), _) if failure.is_some() => {
                self.fail_messages(
                    tx,
                    fx,
                    &format!("没能拉起后端进程：{}", failure.unwrap_or_default()),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn persist(&mut self, tx: &Tx<'_>, fx: &mut Effects) -> nd_store::Result<()> {
        self.show(
            tx,
            fx,
            Shown::Draft {
                draft: self.core.draft.clone(),
            },
        )?;
        let mut header = header(&self.core);
        header["recovering"] = json!(self.recovering());
        self.show(tx, fx, Shown::Header { data: header })?;
        let lineage = &self.core.lineage;
        let rounds: Vec<_> = lineage
            .current()
            .map(|id| lineage.rounds(id).expect("current segment"))
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, turn)| {
                let mut value = serde_json::to_value(turn).expect("round value");
                value["n"] = json!(i + 1);
                value
            })
            .collect();
        let data = json!({"current": lineage.current(), "rounds": rounds, "summarized": self.core.summarized, "inactive_messages": lineage.inactive_messages(), "topology": lineage.topology(),
            "origin": lineage.origin(), "edges": lineage.edges(), "switches": lineage.switches(), "carriers": lineage.carriers(),
            "segments": lineage.topology().iter().map(|n| lineage.segment(&n.segment).unwrap()).collect::<Vec<_>>()});
        self.show(tx, fx, Shown::Lineage { data })?;
        state::save(tx, &self.core)
    }
}
