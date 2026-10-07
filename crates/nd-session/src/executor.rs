//! 会话执行器：每个装载中的会话一个，串行处理它的全部输入，唯一裁决它的一切改动。
//!
//! 一个输入一个事务：命令收据、批次的事实与检查点、发件箱、操作账、独占登记的放行、
//! 显示缓存同一事务提交；提交之后才发事件、给端口确认、把新票交给端口。纯增量不开事务。
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
        let (write_gen, core, born) = match loaded {
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
        if let Input::Batch(batch) = &input
            && batch.facts.is_empty()
            && batch.checkpoint.is_none()
        {
            // 纯增量不开事务（C3）。
            self.publish_live(batch.live.clone());
            return Flow::Continue;
        }
        if let Input::Tick = input
            && !self.idle_due()
        {
            return Flow::Continue;
        }
        // 外部条件变了，但没有在等它的操作或消息：不开事务。
        if let Input::Kick = input
            && self.recovering.is_empty()
            && self.core.ops.is_empty()
            && self.core.messages.values().all(|m| m.ticket.is_some())
            && self.core.invokes.values().all(|i| i.ticket.is_some())
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

    fn after_commit(&mut self, fx: Effects, live: Vec<Live>) {
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
                    if self.recovering() {
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

    fn command(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        fx: &mut Effects,
    ) -> nd_store::Result<Receipt> {
        match command.name.as_str() {
            "session.create" => self.create(tx, command, fx),
            "session.resend" => self.resend(tx, command, fx),
            "session.draft.update" => {
                let (Ok(args), Ok(expected)) = (
                    serde_json::from_value::<nd_wire::DraftUpdate>(command.args.clone()),
                    serde_json::from_value::<nd_wire::DraftExpected>(command.expect.clone()),
                ) else {
                    return Ok(rejected(
                        "invalid",
                        json!({"need":["text", "expect.draft_version"]}),
                    ));
                };
                let attachments = match self.attachments(tx, command) {
                    Ok(a) => a,
                    Err(why) => return Ok(rejected("invalid_attachment", json!({"reason":why}))),
                };
                let owner = if expected.draft_version == self.core.draft.version {
                    let owner = format!("draft/{}", self.id);
                    for a in &self.core.draft.attachments {
                        self.deps.blobs.release(tx, &a.blob, &owner)?;
                    }
                    owner
                } else {
                    format!("draft-saved/{}/{}", self.id, command.id)
                };
                for a in &attachments {
                    self.deps.blobs.hold(tx, &a.blob, &owner)?;
                }
                let result = self.core.update_draft(
                    &command.id,
                    &command.device,
                    expected.draft_version,
                    args.text,
                    attachments,
                );
                Ok(Receipt::Done {
                    value: json!(result),
                })
            }
            "session.send" => {
                if !self.born {
                    return Ok(rejected("not_found", Value::Null));
                }
                self.send(tx, command)
            }
            _ => Ok(rejected("not_found", Value::Null)),
        }
    }

    fn attachments(
        &self,
        tx: &Tx<'_>,
        command: &Command,
    ) -> Result<Vec<nd_wire::Attachment>, String> {
        let attachments: Vec<nd_wire::Attachment> = serde_json::from_value(
            command
                .args
                .get("attachments")
                .cloned()
                .unwrap_or(json!([])),
        )
        .map_err(|e| format!("附件引用格式错误：{e}"))?;
        if attachments.len() > 8 {
            return Err("每条消息最多 8 个附件".into());
        }
        let mut total = 0;
        for a in &attachments {
            a.validate()?;
            if self
                .deps
                .blobs
                .size(tx, &a.blob)
                .map_err(|e| e.to_string())?
                != Some(a.size)
            {
                return Err(format!(
                    "附件 {} 缺失、大小不符或正在清理；请重新上传",
                    a.name
                ));
            }
            total += a.size;
        }
        if total > 16 * 1024 * 1024 {
            return Err("每条消息的附件总大小不能超过 16 MiB".into());
        }
        Ok(attachments)
    }

    fn hold_attachments(
        &self,
        tx: &mut Tx<'_>,
        command: &Command,
        attachments: &[nd_wire::Attachment],
    ) -> nd_store::Result<()> {
        for a in attachments {
            self.deps
                .blobs
                .hold(tx, &a.blob, &format!("message/{}/{}", self.id, command.id))?;
        }
        Ok(())
    }

    fn create(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        fx: &mut Effects,
    ) -> nd_store::Result<Receipt> {
        if self.born {
            // 名册按命令 id 派生会话 id；同 id 不同内容由账本先回 conflict，走不到这里。
            return Ok(rejected("conflict", json!({"session": self.id})));
        }
        let args = &command.args;
        let backend = args["backend"].as_str().unwrap_or("claude");
        let (Some(cwd), Some(text)) = (args["cwd"].as_str(), args["text"].as_str()) else {
            return Ok(rejected("invalid", json!({"need":["cwd","text"]})));
        };
        if !cwd.starts_with('/')
            || (text.trim().is_empty() && args["attachments"].as_array().is_none_or(Vec::is_empty))
        {
            return Ok(rejected(
                "invalid",
                json!({"cwd":"须为绝对路径","text":"不能为空"}),
            ));
        }
        let kind = match backend {
            "claude" => BackendKind::Claude,
            other => return Ok(rejected("unsupported", json!({"backend": other}))),
        };
        if !self.deps.backends.supports(&kind) {
            return Ok(rejected("unsupported", json!({"backend": backend})));
        }
        let attachments = match self.attachments(tx, command) {
            Ok(a) => a,
            Err(why) => return Ok(rejected("invalid_attachment", json!({"reason":why}))),
        };
        self.hold_attachments(tx, command, &attachments)?;
        let optional = |key: &str| args[key].as_str().map(str::to_owned);
        self.core = Core {
            meta: Some(Meta {
                id: self.id.clone(),
                status: Status::Preparing,
                created_by: command.id.clone(),
                cwd: PathBuf::from(cwd),
                kind,
                model: optional("model"),
                permission_mode: optional("permission_mode"),
                note: None,
                irreversible: vec![],
            }),
            ..Core::default()
        };
        self.write_gen = 1;
        state::insert(tx, &self.core, self.write_gen)?;
        self.born = true;
        self.adopt_pending = true;
        let op = self.start_op(
            tx,
            fx,
            OpSpec::Create(Create {
                attachments,
                text: text.to_owned(),
            }),
            Some(command.id.clone()),
        )?;
        Ok(Receipt::Accepted {
            op,
            stream: Some(format!("session/{}", self.id)),
        })
    }

    fn resend(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        fx: &mut Effects,
    ) -> nd_store::Result<Receipt> {
        let Some(id) = command.args["message"].as_str() else {
            return Ok(rejected("invalid", json!({"need":["message"]})));
        };
        let Some(msg) = self.core.undelivered.get(id).cloned() else {
            return Ok(rejected(
                "precondition",
                json!({"message":id,"reason":"尚未证实未送达，或已重发"}),
            ));
        };
        let mut send = command.clone();
        // 重发只重放已确认未送达的原消息，不消费界面当前草稿。
        send.expect = json!({});
        send.args =
            json!({"text":msg.text,"intent":intent_name(msg.intent),"attachments":msg.attachments});
        let receipt = self.send(tx, &send)?;
        if matches!(receipt, Receipt::Done { .. }) {
            self.core.undelivered.remove(id);
            self.show(
                tx,
                fx,
                Shown::Prompt {
                    id: id.into(),
                    text: msg.text,
                    attachments: msg.attachments,
                    intent: intent_name(msg.intent).into(),
                    state: "resent".into(),
                    native: None,
                    reason: Some(format!("已由消息 {} 重发", command.id)),
                },
            )?;
        }
        Ok(receipt)
    }

    fn send(&mut self, tx: &mut Tx<'_>, command: &Command) -> nd_store::Result<Receipt> {
        let status = self.core.meta().status;
        if status == Status::Withdrawn {
            return Ok(rejected("precondition", json!({"status": status.as_str()})));
        }
        let Some(text) = command.args["text"].as_str() else {
            return Ok(rejected("invalid", json!({"need":["text"]})));
        };
        if text.trim().is_empty()
            && command.args["attachments"]
                .as_array()
                .is_none_or(Vec::is_empty)
        {
            return Ok(rejected(
                "invalid",
                json!({"reason":"正文和附件不能同时为空"}),
            ));
        }
        let intent = match command.args["intent"].as_str().unwrap_or("fold") {
            "fold" => Intent::Fold,
            "after_turn" => Intent::AfterTurn,
            "interrupting" => Intent::Interrupting,
            other => return Ok(rejected("invalid", json!({"intent": other}))),
        };
        let attachments = match self.attachments(tx, command) {
            Ok(a) => a,
            Err(why) => return Ok(rejected("invalid_attachment", json!({"reason":why}))),
        };
        self.hold_attachments(tx, command, &attachments)?;
        self.core.arrivals += 1;
        self.core.messages.insert(
            command.id.clone(),
            Message {
                attachments: attachments.clone(),
                id: command.id.clone(),
                text: text.to_owned(),
                intent,
                ticket: None,
                attempt: 0,
                waiting: None,
                arrival: self.core.arrivals,
            },
        );
        // 发送与清稿同一事务；旧界面、不同正文和重试均不能清掉后来编辑的草稿。
        if command.expect["draft_version"].as_u64() == Some(self.core.draft.version)
            && self.core.draft.text == text
            && self.core.draft.attachments == attachments
        {
            for a in &self.core.draft.attachments {
                self.deps
                    .blobs
                    .release(tx, &a.blob, &format!("draft/{}", self.id))?;
            }
            self.core.draft.attachments.clear();
            self.core.draft.version += 1;
            self.core.draft.text.clear();
            self.core.draft.device = command.device.clone();
        }
        // Done 只表示进了发送台；代持、写出、回显看这条消息的状态。
        Ok(Receipt::Done {
            value: json!({"message": command.id, "draft": self.core.draft}),
        })
    }

    fn ensure_lineage(&mut self, carrier: &nd_backend::CarrierId) -> nd_store::Result<()> {
        if self.core.lineage.current().is_none() {
            let c = &self.core.carriers[carrier];
            self.core.lineage = self
                .core
                .lineage
                .fold(&LineageEvent::Root {
                    segment: format!("segment/{}", self.id),
                    carrier: carrier.clone(),
                    backend_session: c.bs.clone(),
                })
                .map_err(aborted)?;
        }
        Ok(())
    }

    fn batch(
        &mut self,
        tx: &mut Tx<'_>,
        batch: Batch,
        fx: &mut Effects,
        _live: &mut Vec<Live>,
    ) -> nd_store::Result<()> {
        if !self.born || !self.core.carriers.contains_key(&batch.carrier) {
            return Ok(());
        }
        for fact in batch.facts {
            match fact.body {
                FactBody::Recovered => {
                    self.recovering.remove(&batch.carrier);
                }
                FactBody::Clarified { ticket, outcome } => {
                    if matches!(
                        outcome,
                        Outcome::Ok {
                            done: Done::Landed { .. }
                        } | Outcome::Refused {
                            refusal: Refusal::Lost { .. }
                        }
                    ) && let Some(mut row) = self.core.uncertain.remove(&ticket)
                    {
                        row.outcome = None;
                        self.core.outbox.insert(ticket.clone(), row);
                        // 原操作若已收场，仅更新原消息证据，不能重新进入正向流程。
                        self.record_outcome(tx, fx, &ticket, outcome)?;
                    }
                }
                FactBody::Done { ticket, outcome } => {
                    self.record_outcome(tx, fx, &ticket, outcome)?
                }
                FactBody::Written { ticket, .. }
                    if self
                        .core
                        .outbox
                        .get(&ticket)
                        .is_some_and(|r| matches!(r.issuer, Issuer::Invoke { .. })) =>
                {
                    let Some(Issuer::Invoke { id }) =
                        self.core.outbox.get(&ticket).map(|r| r.issuer.clone())
                    else {
                        continue;
                    };
                    if let Some(invoke) = self.core.invokes.get(&id).cloned()
                        && invoke.ticket.as_ref() == Some(&ticket)
                    {
                        self.show_invoke(tx, fx, &invoke, "running", None)?;
                    }
                }
                FactBody::CapsChanged {
                    readiness,
                    features,
                } => {
                    if let Some(c) = self.core.carriers.get_mut(&batch.carrier) {
                        c.readiness = Some(readiness);
                        c.features = features;
                    }
                }
                FactBody::Written { ticket, native } => {
                    if let Some(row) = self.core.outbox.get(&ticket)
                        && row.outcome.is_none()
                        && let Act::Send { msg, .. } = &row.act
                        && let Some(display) = row.display.clone()
                    {
                        let shown = Shown::Prompt {
                            id: display,
                            text: msg.text.clone(),
                            attachments: msg.attachments.clone(),
                            intent: intent_name(msg.intent).into(),
                            state: "written".into(),
                            native: Some(native),
                            reason: None,
                        };
                        self.show(tx, fx, shown)?;
                    }
                }
                FactBody::TurnMapped {
                    turn,
                    natives,
                    complete,
                    last_assistant,
                } => {
                    self.ensure_lineage(&batch.carrier)?;
                    self.core.lineage = self
                        .core
                        .lineage
                        .fold(&LineageEvent::TurnObserved {
                            carrier: batch.carrier.clone(),
                            key: turn,
                            natives,
                            complete,
                            last_assistant,
                        })
                        .map_err(aborted)?;
                }
                FactBody::TurnStarted => {
                    if let Some(c) = self.core.carriers.get_mut(&batch.carrier) {
                        c.turn_running = true;
                    }
                }
                FactBody::TurnEnded { ok, subtype, error } => {
                    let n = match self.core.carriers.get_mut(&batch.carrier) {
                        Some(c) => {
                            c.turn_running = false;
                            c.turns += 1;
                            c.turns
                        }
                        None => continue,
                    };
                    self.show(
                        tx,
                        fx,
                        Shown::Turn {
                            carrier: batch.carrier.0.clone(),
                            n,
                            ok,
                            subtype,
                            error,
                        },
                    )?;
                }
                FactBody::Item { item } => self.show(tx, fx, Shown::Block { item })?,
                FactBody::Exited { run, .. } => {
                    if let Some(c) = self.core.carriers.get_mut(&batch.carrier)
                        && c.run.as_ref() == Some(&run)
                    {
                        c.run = None;
                        c.alive = false;
                        c.turn_running = false;
                        c.checkpoint = None;
                    }
                }
                FactBody::Tasks { drain } => {
                    if let Some(c) = self.core.carriers.get_mut(&batch.carrier) {
                        c.drain = drain;
                    }
                }
                FactBody::Gap { lost } => {
                    if lost && let Some(c) = self.core.carriers.get_mut(&batch.carrier) {
                        c.drain = Drain::Unknown {
                            why: "看守流水丢了重建不出的行".into(),
                        };
                    }
                }
                FactBody::Asked { id, kind, raw } => {
                    self.show(tx, fx, Shown::Asked { id, kind, raw })?
                }
            }
        }
        if let Some(checkpoint) = batch.checkpoint
            && let Some(c) = self.core.carriers.get_mut(&batch.carrier)
            && c.alive
        {
            c.checkpoint = Some(checkpoint.clone());
            fx.acks.push((
                c.kind.clone(),
                Ack {
                    session: self.id.clone(),
                    carrier: c.id.clone(),
                    checkpoint,
                },
            ));
        }
        // 检查点会越过本批及此前纯增量：同时保存累积条目，再允许看守回收这些行。
        for Live::Delta { item, kind, text } in batch.live {
            self.show(tx, fx, Shown::Delta { item, kind, text })?;
        }
        for (seq, item) in self.projection.items_with_seq() {
            if item.data["complete"] == false {
                state::put_item(tx, &self.id, &item, seq)?;
            }
        }
        Ok(())
    }

    /// 一张票的终结结果。按票去重：已有结果的再来一次只忽略。
    fn record_outcome(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        ticket: &Ticket,
        outcome: Outcome,
    ) -> nd_store::Result<()> {
        let Some(row) = self.core.outbox.get(ticket).cloned() else {
            return Ok(());
        };
        if row.outcome.is_some() {
            return Ok(());
        }
        // 承载位的进程状态是引擎的事实，不由操作作者写。
        match (&row.act, &outcome) {
            (
                Act::Open { carrier, .. },
                Outcome::Ok {
                    done:
                        Done::Opened {
                            run,
                            readiness,
                            adopt,
                            features,
                            ..
                        },
                },
            ) => {
                if let Some(c) = self.core.carriers.get_mut(carrier) {
                    c.run = Some(run.clone());
                    c.alive = true;
                    c.readiness = Some(readiness.clone());
                    c.features = features.clone();
                    c.adopt = adopt.clone();
                    c.checkpoint = None;
                    c.turn_running = false;
                }
                self.ensure_lineage(carrier)?;
            }
            (Act::End { carrier, .. }, Outcome::Ok { .. }) => {
                if let Some(c) = self.core.carriers.get_mut(carrier) {
                    c.run = None;
                    c.alive = false;
                    c.turn_running = false;
                    c.checkpoint = None;
                }
            }
            _ => {}
        }
        if let (
            Act::Send { to, .. },
            Some(display),
            Outcome::Ok {
                done: Done::Landed { native },
            },
        ) = (&row.act, &row.display, &outcome)
        {
            self.ensure_lineage(to)?;
            self.core.lineage = self
                .core
                .lineage
                .fold(&LineageEvent::Landed {
                    message: display.clone(),
                    ticket: ticket.clone(),
                    position: NativePosition {
                        carrier: to.clone(),
                        backend_session: self.core.carriers[to].bs.clone(),
                        native: native.clone(),
                    },
                })
                .map_err(aborted)?;
        }
        if let (Act::Send { msg, .. }, Some(display)) = (&row.act, row.display.clone())
            && !matches!(
                outcome,
                Outcome::Refused {
                    refusal: Refusal::Withheld
                }
            )
        {
            let (state, native, reason) = match &outcome {
                Outcome::Ok {
                    done: Done::Landed { native },
                } => ("landed", Some(native.clone()), None),
                Outcome::Unknown { .. } => ("unknown", None, Some(outcome.reason())),
                other if !other.possibly_applied() => ("not_delivered", None, Some(other.reason())),
                other => ("failed", None, Some(other.reason())),
            };
            self.show(
                tx,
                fx,
                Shown::Prompt {
                    id: display,
                    text: msg.text.clone(),
                    attachments: msg.attachments.clone(),
                    intent: intent_name(msg.intent).into(),
                    state: state.into(),
                    native,
                    reason,
                },
            )?;
        }
        if let (Act::Send { msg, .. }, Some(display)) = (&row.act, &row.display)
            && !outcome.possibly_applied()
            && !matches!(
                outcome,
                Outcome::Refused {
                    refusal: Refusal::Withheld
                }
            )
        {
            self.core.undelivered.insert(display.clone(), msg.clone());
        }
        if matches!(outcome, Outcome::Unknown { .. }) {
            let mut uncertain = row.clone();
            uncertain.outcome = Some(outcome.clone());
            self.core.uncertain.insert(ticket.clone(), uncertain);
        }
        self.core.outbox.remove(ticket);
        match row.issuer {
            Issuer::Op { op, key } => {
                let Some(mut record) = self.core.ops.remove(&op) else {
                    return Ok(());
                };
                let mut retry = false;
                if let Some(Entry {
                    body:
                        EntryBody::Act {
                            ticket: Some(current),
                            outcome: slot,
                            attempt,
                            ..
                        },
                    ..
                }) = record.entries.get_mut(&key)
                    && current == ticket
                {
                    if matches!(
                        outcome,
                        Outcome::Refused {
                            refusal: Refusal::Withheld
                        }
                    ) && matches!(record.phase, Phase::Running)
                    {
                        // 证明没写出（G3）：同一个键另发一张票。
                        *attempt += 1;
                        retry = true;
                    } else {
                        *slot = Some(outcome);
                    }
                }
                if retry
                    && let Some(Entry {
                        body: EntryBody::Act { ticket, .. },
                        ..
                    }) = record.entries.get_mut(&key)
                {
                    *ticket = None;
                }
                self.core.ops.insert(op, record);
            }
            Issuer::Invoke { id } => {
                if matches!(
                    outcome,
                    Outcome::Refused {
                        refusal: Refusal::Withheld
                    }
                ) {
                    // 证明没写出：回到等待，按当下事实重新放行、另发尝试。
                    if let Some(i) = self.core.invokes.get_mut(&id) {
                        i.ticket = None;
                        i.attempt += 1;
                    }
                } else {
                    self.finish_invoke(tx, fx, &id, outcome)?;
                }
            }
            Issuer::Message { id } => {
                if matches!(
                    outcome,
                    Outcome::Refused {
                        refusal: Refusal::Withheld
                    }
                ) {
                    // 证明没写出：消息回发送台，另发尝试，界面上仍是这一条。
                    if let Some(m) = self.core.messages.get_mut(&id) {
                        m.ticket = None;
                        m.attempt += 1;
                    }
                } else {
                    self.core.messages.remove(&id);
                }
            }
        }
        Ok(())
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
            let mut progress = false;
            let mut ids: Vec<String> = self.core.ops.keys().cloned().collect();
            ids.sort_by_key(|id| id.rsplit(':').next().and_then(|n| n.parse::<u64>().ok()));
            for id in ids {
                progress |= self.advance(tx, fx, &id)?;
            }
            progress |= self.pump(tx, fx)?;
            progress |= self.pump_invokes(tx, fx)?;
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
        if let Act::Send { .. } = &*act {
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
                    for attachment in &create.attachments {
                        self.deps.blobs.release(
                            tx,
                            &attachment.blob,
                            &format!("message/{}/{}", self.id, self.core.meta().created_by),
                        )?;
                    }
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

    fn fail_messages(
        &mut self,
        tx: &Tx<'_>,
        fx: &mut Effects,
        reason: &str,
    ) -> nd_store::Result<()> {
        let queued: Vec<Message> = self
            .core
            .messages
            .values()
            .filter(|m| m.ticket.is_none())
            .cloned()
            .collect();
        for m in queued {
            self.core.messages.remove(&m.id);
            self.show(
                tx,
                fx,
                Shown::Prompt {
                    id: m.id.clone(),
                    text: m.text.clone(),
                    attachments: m.attachments.clone(),
                    intent: intent_name(m.intent).into(),
                    state: "failed".into(),
                    native: None,
                    reason: Some(reason.into()),
                },
            )?;
        }
        Ok(())
    }

    // —— 发送台 ——

    /// 发送台：结构操作进行中代持；当前承载位没有活进程就起按需拉起；
    /// 否则按到达次序经独占登记放行写出（G12）。
    fn pump(&mut self, tx: &mut Tx<'_>, fx: &mut Effects) -> nd_store::Result<bool> {
        let mut queued: Vec<Message> = self
            .core
            .messages
            .values()
            .filter(|m| m.ticket.is_none())
            .cloned()
            .collect();
        if queued.is_empty() {
            return Ok(false);
        }
        queued.sort_by_key(|m| m.arrival);
        let structural = !self.core.ops.is_empty();
        let mut progress = false;
        for m in queued {
            let current = self.core.current_carrier().cloned();
            // 结构操作进行中，或还没有当前承载位（创建中）：代持。
            let hold_reason = (structural || current.is_none()).then_some("held");
            if let Some(state) = hold_reason {
                self.show_message(tx, fx, &m, state, None)?;
                continue;
            }
            let carrier = current.unwrap();
            if !carrier.alive {
                self.show_message(tx, fx, &m, "held", None)?;
                self.start_op(
                    tx,
                    fx,
                    OpSpec::Launch(Launch {
                        carrier: carrier.id.clone(),
                    }),
                    None,
                )?;
                return Ok(true);
            }
            let ticket = Ticket(format!("m:{}#{}", m.id, m.attempt));
            let decision = self.deps.claims.admit(
                tx,
                &ticket.0,
                &nd_claims::Act::Write {
                    session: self.id.0.clone(),
                    bs: carrier.bs.clone(),
                },
            )?;
            match decision {
                nd_claims::Admit::Go(nd_claims::Pass {
                    route: nd_claims::Route::Live(run),
                }) if carrier.run.as_ref().is_some_and(|r| r.0 == run) => {
                    let row = OutRow {
                        issued: Issued {
                            ticket: ticket.clone(),
                            session: self.id.clone(),
                            write_gen: self.write_gen,
                        },
                        act: Act::Send {
                            to: carrier.id.clone(),
                            msg: nd_backend::Msg {
                                text: m.text.clone(),
                                attachments: m.attachments.clone(),
                                intent: m.intent,
                            },
                        },
                        kind: carrier.kind.clone(),
                        issuer: Issuer::Message { id: m.id.clone() },
                        display: Some(m.id.clone()),
                        outcome: None,
                        handed: false,
                    };
                    self.core.outbox.insert(ticket.clone(), row);
                    if let Some(msg) = self.core.messages.get_mut(&m.id) {
                        msg.ticket = Some(ticket.clone());
                        msg.waiting = None;
                    }
                    fx.hand.push(ticket);
                    self.show_message(tx, fx, &m, "pending", None)?;
                    progress = true;
                }
                nd_claims::Admit::Go(_) => {
                    self.show_message(tx, fx, &m, "held", None)?;
                }
                nd_claims::Admit::Wait(obstacle) => {
                    let why = format!("{obstacle:?}");
                    if let Some(msg) = self.core.messages.get_mut(&m.id) {
                        msg.waiting = Some(why.clone());
                    }
                    self.show_message(tx, fx, &m, "waiting", Some(why))?;
                }
                nd_claims::Admit::No(refusal) => {
                    self.core.messages.remove(&m.id);
                    self.show_message(
                        tx,
                        fx,
                        &m,
                        "failed",
                        Some(format!("独占登记不放行：{refusal:?}")),
                    )?;
                    progress = true;
                }
            }
        }
        Ok(progress)
    }

    fn show_message(
        &mut self,
        tx: &Tx<'_>,
        fx: &mut Effects,
        m: &Message,
        state: &str,
        reason: Option<String>,
    ) -> nd_store::Result<()> {
        self.show(
            tx,
            fx,
            Shown::Prompt {
                id: m.id.clone(),
                text: m.text.clone(),
                attachments: m.attachments.clone(),
                intent: intent_name(m.intent).into(),
                state: state.into(),
                native: None,
                reason,
            },
        )
    }

    // —— 总结、`!` 命令、fork 型子代理（收据等动作有结果）——

    /// 受理一条收据等动作有结果的命令：校验、记下意图（核心里的 `Invoke`）、按需清掉匹配的草稿。
    /// 返回 `Some` 是立即可定的拒绝收据；`None` 表示已受理，收据等结果。
    fn invoke_command(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        fx: &mut Effects,
    ) -> nd_store::Result<Option<Receipt>> {
        if !self.born {
            return Ok(Some(rejected("not_found", Value::Null)));
        }
        let status = self.core.meta().status;
        if status == Status::Withdrawn {
            return Ok(Some(rejected(
                "precondition",
                json!({"status": status.as_str()}),
            )));
        }
        let feature = match command.name.as_str() {
            "session.shell" => "bang_mode",
            "session.subtask" => "fork_subagent",
            _ => "summarize",
        };
        if let Some(why) = self
            .core
            .current_carrier()
            .and_then(|c| c.unavailable(feature))
        {
            // 降级的进程（mod 没装上、只能聊天）做不了：会话头已写明原因。
            return Ok(Some(rejected(
                "unsupported",
                json!({"feature": feature, "why": why}),
            )));
        }
        let args = &command.args;
        let mut message = None;
        let mut restore = None;
        let mut covers = vec![];
        let mut input = None;
        let invocation = match command.name.as_str() {
            "session.shell" => match serde_json::from_value::<nd_wire::ShellArgs>(args.clone()) {
                Ok(a) if !a.command.trim().is_empty() => {
                    input = a.input;
                    Invocation::Shell {
                        command: a.command.trim().to_owned(),
                    }
                }
                _ => return Ok(Some(rejected("invalid", json!({"need":["command"]})))),
            },
            "session.subtask" => {
                match serde_json::from_value::<nd_wire::SubtaskArgs>(args.clone()) {
                    Ok(a) if !a.prompt.trim().is_empty() => {
                        input = a.input;
                        Invocation::ForkAgent {
                            prompt: a.prompt.trim().to_owned(),
                        }
                    }
                    _ => return Ok(Some(rejected("invalid", json!({"need":["prompt"]})))),
                }
            }
            _ => {
                let Ok(a) = serde_json::from_value::<nd_wire::CompactArgs>(args.clone()) else {
                    return Ok(Some(rejected(
                        "invalid",
                        json!({"need":["message","scope: from | up_to"]}),
                    )));
                };
                let scope = match a.scope {
                    nd_wire::CompactScope::From => CompactScope::From,
                    nd_wire::CompactScope::UpTo => CompactScope::UpTo,
                };
                let target = a.message.as_str();
                match self.anchor(target, scope) {
                    Ok((anchor, text, attachments, covered)) => {
                        message = Some(target.to_owned());
                        if scope == CompactScope::From {
                            restore = Some((text, attachments));
                        }
                        covers = covered;
                        Invocation::Compact { scope, anchor }
                    }
                    Err(reason) => {
                        return Ok(Some(rejected(
                            "precondition",
                            json!({"message": target, "reason": reason}),
                        )));
                    }
                }
            }
        };
        let mut invoke = Invoke {
            id: command.id.clone(),
            digest: command.content_hash(),
            device: command.device.clone(),
            invocation,
            message,
            restore,
            draft_base: self.core.draft.version,
            covers,
            ticket: None,
            attempt: 0,
            waiting: None,
            arrival: 0,
        };
        if matches!(invoke.invocation, Invocation::Compact { .. })
            && self
                .core
                .invokes
                .values()
                .any(|i| matches!(i.invocation, Invocation::Compact { .. }))
        {
            return Ok(Some(rejected(
                "precondition",
                json!({"reason": "已有一次总结在进行，等它结束再选"}),
            )));
        }
        self.core.arrivals += 1;
        invoke.arrival = self.core.arrivals;
        // `!` 和 /subtask 从输入框发出：输入框原文（`input`）等于当前稿且版本对得上，就同事务清稿。
        if let Some(input) = input.as_deref()
            && command.expect["draft_version"].as_u64() == Some(self.core.draft.version)
            && self.core.draft.text == input
            && self.core.draft.attachments.is_empty()
        {
            self.core.draft.version += 1;
            self.core.draft.text.clear();
            self.core.draft.device = command.device.clone();
            invoke.draft_base = self.core.draft.version;
        }
        let shown = invoke_shown(&invoke, "held", json!({}));
        self.core.invokes.insert(invoke.id.clone(), invoke);
        self.show(tx, fx, shown)?;
        Ok(None)
    }

    /// 当前段里还是 CLI 对话行的人类提示，按出现先后。
    fn visible_prompts(&self) -> Vec<String> {
        let lineage = &self.core.lineage;
        lineage
            .current()
            .and_then(|segment| lineage.turns(segment).ok())
            .unwrap_or_default()
            .iter()
            .flat_map(|turn| turn.messages.clone())
            .filter(|m| !self.core.summarized.contains(m))
            .collect()
    }

    fn prompt_content(&self, message: &str) -> Option<(String, Vec<nd_wire::Attachment>)> {
        let id = format!("prompt/{message}");
        self.projection
            .items()
            .into_iter()
            .find(|i| i.id == id)
            .map(|i| {
                (
                    i.data["text"].as_str().unwrap_or_default().to_owned(),
                    serde_json::from_value(i.data["attachments"].clone()).unwrap_or_default(),
                )
            })
    }

    /// 所选提示的定位（原文与同一原文里的次序）和这次总结覆盖的提示；不能总结时回原因。
    #[allow(clippy::type_complexity)]
    fn anchor(
        &self,
        message: &str,
        scope: CompactScope,
    ) -> Result<
        (
            nd_backend::Anchor,
            String,
            Vec<nd_wire::Attachment>,
            Vec<String>,
        ),
        String,
    > {
        if self.core.summarized.contains(message) {
            return Err("这条提示已经被总结过".into());
        }
        let visible = self.visible_prompts();
        let Some(at) = visible.iter().position(|m| m == message) else {
            return Err("这条提示不在当前对话里，或还没有送达".into());
        };
        if scope == CompactScope::UpTo && at == 0 {
            return Err("这条提示之前没有可总结的内容".into());
        }
        let Some((text, attachments)) = self.prompt_content(message) else {
            return Err("找不到这条提示的原文".into());
        };
        let same: Vec<&String> = visible
            .iter()
            .filter(|m| {
                self.prompt_content(m).as_ref() == Some(&(text.clone(), attachments.clone()))
            })
            .collect();
        let nth = same.iter().position(|m| *m == message).unwrap_or(0) as u32 + 1;
        let covers = match scope {
            CompactScope::From => visible[at..].to_vec(),
            CompactScope::UpTo => visible[..at].to_vec(),
        };
        Ok((
            nd_backend::Anchor {
                text: text.clone(),
                attachments: attachments.clone(),
                nth,
                of: same.len() as u32,
            },
            text,
            attachments,
            covers,
        ))
    }

    /// 发送台的另一半：按到达次序签发等着的总结、`!` 命令和 fork 型子代理。
    /// 结构操作中、没有当前承载位时代持；当前承载位没有活进程就起按需拉起；`!` 等当前回合结束再跑。
    fn pump_invokes(&mut self, tx: &mut Tx<'_>, fx: &mut Effects) -> nd_store::Result<bool> {
        let mut queued: Vec<Invoke> = self
            .core
            .invokes
            .values()
            .filter(|i| i.ticket.is_none())
            .cloned()
            .collect();
        if queued.is_empty() {
            return Ok(false);
        }
        queued.sort_by_key(|i| i.arrival);
        let mut progress = false;
        for invoke in queued {
            let structural = !self.core.ops.is_empty();
            let current = self.core.current_carrier().cloned();
            let Some(carrier) = current.filter(|_| !structural) else {
                self.show_invoke(tx, fx, &invoke, "held", None)?;
                continue;
            };
            if !carrier.alive {
                self.show_invoke(tx, fx, &invoke, "held", None)?;
                self.start_op(
                    tx,
                    fx,
                    OpSpec::Launch(Launch {
                        carrier: carrier.id.clone(),
                    }),
                    None,
                )?;
                return Ok(true);
            }
            if let Some(why) = carrier.unavailable(invoke.feature()) {
                self.finish_invoke(
                    tx,
                    fx,
                    &invoke.id,
                    Outcome::Rejected {
                        reject: nd_backend::Reject::Unsupported { why },
                    },
                )?;
                progress = true;
                continue;
            }
            if matches!(invoke.invocation, Invocation::Shell { .. }) && carrier.turn_running {
                // 和终端一样：回合进行中输入的 `!` 等这一回合结束再跑。
                self.show_invoke(tx, fx, &invoke, "waiting_turn", None)?;
                continue;
            }
            let ticket = Ticket(format!("i:{}#{}", invoke.id, invoke.attempt));
            match self.deps.claims.admit(
                tx,
                &ticket.0,
                &nd_claims::Act::Write {
                    session: self.id.0.clone(),
                    bs: carrier.bs.clone(),
                },
            )? {
                nd_claims::Admit::Go(nd_claims::Pass {
                    route: nd_claims::Route::Live(run),
                }) if carrier.run.as_ref().is_some_and(|r| r.0 == run) => {
                    let row = OutRow {
                        issued: Issued {
                            ticket: ticket.clone(),
                            session: self.id.clone(),
                            write_gen: self.write_gen,
                        },
                        act: Act::Invoke {
                            to: carrier.id.clone(),
                            what: invoke.invocation.clone(),
                        },
                        kind: carrier.kind.clone(),
                        issuer: Issuer::Invoke {
                            id: invoke.id.clone(),
                        },
                        display: None,
                        outcome: None,
                        handed: false,
                    };
                    self.core.outbox.insert(ticket.clone(), row);
                    if let Some(i) = self.core.invokes.get_mut(&invoke.id) {
                        i.ticket = Some(ticket.clone());
                        i.waiting = None;
                    }
                    fx.hand.push(ticket);
                    self.show_invoke(tx, fx, &invoke, "pending", None)?;
                    progress = true;
                }
                nd_claims::Admit::Go(_) => self.show_invoke(tx, fx, &invoke, "held", None)?,
                nd_claims::Admit::Wait(obstacle) => {
                    let why = format!("{obstacle:?}");
                    if let Some(i) = self.core.invokes.get_mut(&invoke.id) {
                        i.waiting = Some(why.clone());
                    }
                    self.show_invoke(tx, fx, &invoke, "waiting", Some(why))?;
                }
                nd_claims::Admit::No(refusal) => {
                    self.finish_invoke(
                        tx,
                        fx,
                        &invoke.id,
                        Outcome::Refused {
                            refusal: Refusal::Other {
                                why: format!("独占登记不放行：{refusal:?}"),
                            },
                        },
                    )?;
                    progress = true;
                }
            }
        }
        Ok(progress)
    }

    fn show_invoke(
        &mut self,
        tx: &Tx<'_>,
        fx: &mut Effects,
        invoke: &Invoke,
        state: &str,
        reason: Option<String>,
    ) -> nd_store::Result<()> {
        let shown = invoke_shown(invoke, state, json!({"reason": reason}));
        self.show(tx, fx, shown)
    }

    /// 一件 Invoke 有了结果：落收据（同事务），更新条目；总结成功时记下被总结的提示并回填草稿。
    fn finish_invoke(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        id: &str,
        outcome: Outcome,
    ) -> nd_store::Result<()> {
        let Some(invoke) = self.core.invokes.remove(id) else {
            return Ok(());
        };
        let reason = outcome.reason();
        let (receipt, state, mut extra) = match &outcome {
            Outcome::Ok {
                done: Done::Invoked { result },
            } => {
                let value = match result {
                    Invoked::Compacted => json!({}),
                    Invoked::Shell {
                        exit,
                        stdout,
                        stderr,
                        appended,
                    } => {
                        json!({"exit": exit, "stdout": stdout, "stderr": stderr, "appended": appended})
                    }
                    Invoked::Forked { agent } => json!({"agent": agent}),
                };
                (None, "done", value)
            }
            Outcome::Ok { .. } => (None, "done", json!({})),
            Outcome::Rejected { reject } => {
                let code = match reject {
                    nd_backend::Reject::Unsupported { .. } => "unsupported",
                    nd_backend::Reject::Gone | nd_backend::Reject::NoCarrier => "precondition",
                    nd_backend::Reject::Invalid { .. } => "invalid",
                    nd_backend::Reject::NotAdopted | nd_backend::Reject::Busy => "unavailable",
                    nd_backend::Reject::Conflict => "internal",
                };
                (Some(code), "rejected", json!({"reason": reason}))
            }
            Outcome::Refused {
                refusal: Refusal::AnchorGone { why },
            } => (Some("anchor_gone"), "rejected", json!({"reason": why})),
            Outcome::Refused { .. } => {
                (Some("not_executed"), "rejected", json!({"reason": reason}))
            }
            Outcome::Failed { .. } => (Some("failed"), "failed", json!({"reason": reason})),
            Outcome::Unknown { .. } => (Some("unknown"), "unknown", json!({"reason": reason})),
        };
        if state == "done"
            && let Invocation::Compact { scope, .. } = &invoke.invocation
        {
            self.core.summarized.extend(invoke.covers.iter().cloned());
            let (text, attachments) = match scope {
                CompactScope::From => invoke.restore.clone().unwrap_or_default(),
                CompactScope::UpTo => (String::new(), vec![]),
            };
            let saved = self.backfill(tx, &invoke, text, attachments)?;
            extra["saved"] = json!(saved);
        }
        extra["invoke"] = json!(invoke.id);
        let mut value = extra.clone();
        value["draft"] = json!(self.core.draft);
        let receipt = match receipt {
            None => Receipt::Done {
                // 成功收据的形状只在 nd-wire 定义一次（`Invoked`）。
                value: serde_json::to_value(
                    serde_json::from_value::<nd_wire::Invoked>(value)
                        .map_err(|e| aborted(format!("Invoked 收据：{e}")))?,
                )
                .expect("Invoked serializes"),
            },
            Some("unknown") => Receipt::Unknown {
                now: json!({"invoke": invoke.id, "stream": format!("session/{}", self.id)}),
            },
            Some(code) => rejected(code, value),
        };
        let keep = self.deps.config.receipt_keep_ms;
        nd_ledger::record(tx, &invoke.id, &invoke.digest, &receipt, keep)?;
        let shown = invoke_shown(&invoke, state, extra);
        self.show(tx, fx, shown)?;
        fx.replies
            .push((invoke.id.clone(), CommandReply::Receipt { receipt }));
        Ok(())
    }

    /// 总结之后的草稿回填（#16 的回填约定）：以受理时的版本为基准；之后改过的不覆盖，
    /// 回填的原文另存。被替换掉的非空草稿也另存，不丢。返回另存稿的 id。
    fn backfill(
        &mut self,
        tx: &mut Tx<'_>,
        invoke: &Invoke,
        text: String,
        attachments: Vec<nd_wire::Attachment>,
    ) -> nd_store::Result<Vec<String>> {
        let draft_owner = format!("draft/{}", self.id);
        let mut saved = vec![];
        if invoke.draft_base != self.core.draft.version {
            if text.is_empty() && attachments.is_empty() {
                // 「总结到这里」留空：输入框已经被改过，就不动它。
                return Ok(saved);
            }
            let id = format!("{}/backfill", invoke.id);
            for a in &attachments {
                self.deps
                    .blobs
                    .hold(tx, &a.blob, &format!("draft-saved/{}/{id}", self.id))?;
            }
            self.core
                .update_draft(&id, &invoke.device, invoke.draft_base, text, attachments);
            saved.push(id);
            return Ok(saved);
        }
        let old = self.core.draft.clone();
        if (!old.text.is_empty() || !old.attachments.is_empty())
            && (old.text != text || old.attachments != attachments)
        {
            let id = format!("{}/displaced", invoke.id);
            for a in &old.attachments {
                self.deps
                    .blobs
                    .hold(tx, &a.blob, &format!("draft-saved/{}/{id}", self.id))?;
            }
            self.core.draft.saved.push(nd_wire::SavedDraft {
                attachments: old.attachments.clone(),
                id: id.clone(),
                base_version: old.version,
                text: old.text.clone(),
                device: old.device.clone(),
            });
            saved.push(id);
        }
        for a in &old.attachments {
            self.deps.blobs.release(tx, &a.blob, &draft_owner)?;
        }
        for a in &attachments {
            self.deps.blobs.hold(tx, &a.blob, &draft_owner)?;
        }
        self.core.update_draft(
            &format!("{}/backfill", invoke.id),
            &invoke.device,
            invoke.draft_base,
            text,
            attachments,
        );
        Ok(saved)
    }

    // —— 闲置回收 ——

    /// 当前进程闲置：没有操作、没有回合、发送台空、没有未结的票、后台任务确知已收尾、没人在看。
    fn idle_now(&self) -> bool {
        let Some(meta) = &self.core.meta else {
            return false;
        };
        let Some(carrier) = self.core.current_carrier() else {
            return false;
        };
        matches!(meta.status, Status::Active | Status::Partial)
            && self.core.ops.is_empty()
            && self.core.messages.is_empty()
            && self.core.invokes.is_empty()
            && self.core.outbox.is_empty()
            && carrier.alive
            && !carrier.turn_running
            && carrier.drain == Drain::Drained
            && self.shared.watchers.load(Ordering::Acquire) == 0
    }

    fn idle_due(&mut self) -> bool {
        if !self.idle_now() {
            self.idle_since = None;
            return false;
        }
        let since = *self.idle_since.get_or_insert_with(Instant::now);
        since.elapsed() >= self.deps.config.idle_reclaim
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
            .map(|id| lineage.turns(id).expect("current segment"))
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

/// `invoke/<命令 id>` 条目：种类、内容、状态，再并上结果字段。
fn invoke_shown(invoke: &Invoke, state: &str, extra: Value) -> Shown {
    let mut data = match &invoke.invocation {
        Invocation::Shell { command } => json!({"command": command}),
        Invocation::ForkAgent { prompt } => json!({"prompt": prompt}),
        Invocation::Compact { scope, .. } => json!({
            "scope": match scope { CompactScope::From => "from", CompactScope::UpTo => "up_to" },
            "message": invoke.message,
        }),
    };
    data["invoke"] = json!(invoke.id);
    data["state"] = json!(state);
    if let Value::Object(extra) = extra {
        for (k, v) in extra {
            if !v.is_null() {
                data[k] = v;
            }
        }
    }
    Shown::Invoke {
        id: invoke.id.clone(),
        kind: invoke.kind().into(),
        data,
    }
}

fn intent_name(intent: Intent) -> &'static str {
    match intent {
        Intent::Fold => "fold",
        Intent::AfterTurn => "after_turn",
        Intent::Interrupting => "interrupting",
    }
}

pub(crate) fn header(core: &Core) -> Value {
    let meta = core.meta();
    let carrier = core
        .current_carrier()
        .or_else(|| core.carriers.values().next());
    json!({
        "session": meta.id,
        "status": meta.status.as_str(),
        "created_by": meta.created_by,
        "cwd": meta.cwd,
        "backend": format!("{:?}", meta.kind).to_lowercase(),
        "model": meta.model,
        "permission_mode": meta.permission_mode,
        "note": meta.note,
        "irreversible": meta.irreversible,
        "process": carrier.map(|c| json!({
            "carrier": c.id,
            "backend_session": c.bs.id,
            "run": c.run,
            "alive": c.alive,
            "readiness": c.readiness,
            "turn_running": c.turn_running,
            "drain": c.drain,
            "features": c.features,
        })),
        // 只能聊天的降级进程：会话头写明原因和这时用不了的功能（端口能力表给的名字）。
        "degraded": carrier.and_then(|c| match &c.readiness {
            Some(nd_backend::Readiness::ChatOnly { why }) => Some(json!({
                "why": why,
                "unavailable": c.features.iter().filter(|f| !f.available).map(|f| f.label.clone()).collect::<Vec<_>>(),
            })),
            _ => None,
        }),
        "op": core.ops.values().next().map(|op| json!({"op": op.id, "kind": op.spec.kind(), "phase": op.phase.name()})),
    })
}
