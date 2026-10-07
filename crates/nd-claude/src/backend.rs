//! Claude 适配：后端端口 [`BackendAdapter`] 的 Claude 实现。
//!
//! 每个活的承载位一个任务，持有它的看守连接（唯一的 stdin 写入者）：交来的票按到达次序写出，
//! 定时读看守流水，经 [`Conversation`] 归一成事实，按批交到会话的收件地址；
//! 会话提交了带检查点的批次之后才给看守确认。拉起、退出都向独占登记报观察（`Up`、`Gone`）。
//!
//! 我方发起的原生编号由票派生（user 行的 uuid 是 `native_uuid(票)`），送达只认带原 uuid 的回显。
use crate::{
    Caps, Claude, ClaudeRun, CommandResult, Feature, InitOptions, ModChannel, Open,
    Readiness as ClaudeReadiness, Start,
    convo::{Conversation, Convo},
    invoke,
};
use nd_backend::{
    Ack, Act, Admit, AdoptPart, BackendAdapter, BackendKind, Batch, CarrierId, CarrierRecord,
    Checkpoint, Done, Drain, EndHow, Fact, FactBody, Inbox, Intent, Invocation, Issued, Live, Msg,
    OpenSpec, Origin, Outcome, PendingTicket, Readiness, Refusal, Reject, RunId, SessionId, Ticket,
    native_uuid,
};
use nd_claims::{Exclusivity, GoneHow, Observed};
use nd_mod_proto::{Action, ModName, OpPhase, QueryAnswer};
use nd_runs::{Found, Watchdogs};
use nd_watchdog_proto::{Event, Finish};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub struct ClaudeBackendConfig {
    /// 空闲时多久读一次看守流水。
    pub poll: Duration,
    /// 后端进程退出后等看守单元清理完（独占登记据此放掉租约）的时限。
    pub gone_timeout: Duration,
    /// 录下读到的看守流水（场景测试与录制回归用）：每个后端进程一个 `<run>.jsonl`，
    /// 在给看守确认、流水被回收之前写。生产默认不录。
    pub record_dir: Option<std::path::PathBuf>,
}
impl Default for ClaudeBackendConfig {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(20),
            gone_timeout: Duration::from_secs(15),
            record_dir: None,
        }
    }
}

struct NormalPermit(Arc<AtomicUsize>);
impl Drop for NormalPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

enum Cmd {
    Send {
        ticket: Ticket,
        msg: Msg,
        _permit: NormalPermit,
    },
    End {
        ticket: Ticket,
        how: EndHow,
    },
    Control {
        ticket: Ticket,
        act: Act,
    },
    Ack(u64),
    Title {
        ticket: Ticket,
        invocation: nd_backend::Invocation,
        write: bool,
    },
    Configure {
        ticket: Ticket,
        setting: nd_wire::LiveSetting,
    },
    /// 总结、`!`、派 fork 型子代理：经动作 mod 做（改标题、生成标题走 `Title`）。
    Invoke {
        ticket: Ticket,
        invocation: Invocation,
    },
    /// 动作 mod 对一条 Invoke 的结论（或查不到）。
    Concluded {
        op_id: String,
        result: Option<CommandResult>,
    },
}

/// 交给动作 mod、还没有结论的一件事。随检查点保存，守护进程重启后按操作 id 查。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingInvoke {
    ticket: Ticket,
    invocation: Invocation,
    /// `!` 命令还没用掉的自动批准（命令原文）；批准过一次就清掉。
    approval: Option<String>,
}

/// 等动作 mod 结论的上限：Bash 最长 10 分钟，压缩要等会话空闲。
const INVOKE_WAIT: Duration = Duration::from_secs(3600);

/// 改标题、生成标题走控制请求（`Cmd::Title`）；其余 Invoke 经动作 mod。
fn titles(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Title { .. } | Invocation::GenerateTitle { .. }
    )
}

fn features(caps: &Caps) -> Vec<nd_backend::Feature> {
    caps.table()
}

fn readiness_of(caps: &Caps) -> Readiness {
    match &caps.readiness {
        ClaudeReadiness::Full => Readiness::Full,
        ClaudeReadiness::ChatOnly { why } => Readiness::ChatOnly { why: why.clone() },
    }
}

/// 读附件、按 Claude 的内容块编码一条消息（发送与总结定位共用）。
fn encode(blobs: &nd_store::Blobs, message: &Msg) -> Result<Vec<Value>, String> {
    use base64::Engine;
    let mut content = vec![];
    if !message.text.is_empty() {
        content.push(json!({"type":"text","text":message.text}));
    }
    for attachment in &message.attachments {
        let bytes = blobs.get(&attachment.blob).map_err(|e| e.to_string())?;
        if attachment.media_type == "text/plain" {
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("{} 不是 UTF-8 文本", attachment.name))?;
            content.push(json!({"type":"text","text":format!("附件 {}：\n{}", serde_json::to_string(&attachment.name).unwrap(), text)}));
        } else {
            let kind = if attachment.media_type == "application/pdf" {
                "document"
            } else {
                "image"
            };
            content.push(json!({"type":kind,"source":{"type":"base64","media_type":attachment.media_type,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}}));
        }
    }
    Ok(content)
}

/// 守护进程重启后按操作 id 问动作 mod：做完了取结论，还在跑就过一会儿再问，查不到就是不明。
async fn recheck(channel: ModChannel, run: String, op_id: String, me: mpsc::UnboundedSender<Cmd>) {
    loop {
        let query = channel.binding(&run).and_then(|binding| {
            let hello = binding.mods.get(&ModName::Actions)?;
            let id = uuid::Uuid::new_v4().to_string();
            channel
                .send(
                    &run,
                    ModName::Actions,
                    nd_mod_proto::Command {
                        op_id: id.clone(),
                        expected_backend_session_id: binding.backend_session_id.clone(),
                        expected_mod_gen: hello.mod_gen.clone(),
                        action: Action::Query {
                            op_ids: vec![op_id.clone()],
                        },
                    },
                )
                .then_some(id)
        });
        let answer = match query {
            Some(id) => channel.result(&run, &id, Duration::from_secs(15)).await,
            None => None,
        };
        let state = match answer {
            Some(CommandResult::Outcome(nd_mod_proto::Outcome::Done { value })) => {
                serde_json::from_value::<QueryAnswer>(value)
                    .ok()
                    .and_then(|a| a.ops.into_iter().find(|o| o.op_id == op_id))
            }
            _ => None,
        };
        match state {
            Some(op) if op.phase == OpPhase::Running => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Some(op) if op.phase == OpPhase::Done => {
                let _ = me.send(Cmd::Concluded {
                    op_id,
                    result: op.outcome.map(CommandResult::Outcome),
                });
                return;
            }
            _ => {
                let _ = me.send(Cmd::Concluded {
                    op_id,
                    result: Some(CommandResult::Unknown),
                });
                return;
            }
        }
    }
}

struct Slot {
    session: SessionId,
    tx: mpsc::UnboundedSender<Cmd>,
    normal_queued: Arc<AtomicUsize>,
}

struct Inner {
    claude: Claude,
    watchdogs: Arc<Watchdogs>,
    claims: Arc<Exclusivity>,
    generation: u64,
    runtime: tokio::runtime::Handle,
    config: ClaudeBackendConfig,
    blobs: Arc<nd_store::Blobs>,
    inboxes: Mutex<HashMap<SessionId, Inbox>>,
    carriers: Mutex<HashMap<CarrierId, Slot>>,
    model_query: tokio::sync::Mutex<()>,
}

pub struct ClaudeBackend {
    inner: Arc<Inner>,
}

fn fact(key: String, body: FactBody) -> Fact {
    Fact { key, body }
}
fn clarified(ticket: &Ticket, outcome: Outcome) -> Fact {
    fact(
        format!("clarified:{ticket}"),
        FactBody::Clarified {
            ticket: ticket.clone(),
            outcome,
        },
    )
}
fn done(ticket: &Ticket, outcome: Outcome) -> Fact {
    fact(
        format!("done:{ticket}"),
        FactBody::Done {
            ticket: ticket.clone(),
            outcome,
        },
    )
}

impl ClaudeBackend {
    /// `generation`：这一代守护进程报给独占登记的控制代次（重启后更大）。
    /// 要在 Tokio 运行时里构造：后端进程的任务跑在这个运行时上。
    pub fn new(
        claude: Claude,
        watchdogs: Arc<Watchdogs>,
        claims: Arc<Exclusivity>,
        generation: u64,
        blobs: Arc<nd_store::Blobs>,
        config: ClaudeBackendConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner {
                claude,
                watchdogs,
                claims,
                generation,
                runtime: tokio::runtime::Handle::current(),
                config,
                blobs,
                inboxes: Mutex::new(HashMap::new()),
                carriers: Mutex::new(HashMap::new()),
                model_query: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub fn claude(&self) -> &Claude {
        &self.inner.claude
    }
}

impl Inner {
    async fn deliver(&self, session: &SessionId, batch: Batch) {
        let inbox = self.inboxes.lock().unwrap().get(session).cloned();
        if let Some(inbox) = inbox {
            // 会话卸下或执行器停了：丢掉，重新装载时 adopt 会对账。
            let _ = inbox.send(batch).await;
        }
    }

    async fn deliver_facts(&self, session: &SessionId, carrier: &CarrierId, facts: Vec<Fact>) {
        self.deliver(
            session,
            Batch {
                carrier: carrier.clone(),
                facts,
                live: vec![],
                checkpoint: None,
            },
        )
        .await;
    }

    async fn inspect(&self, run: &RunId) -> Option<Found> {
        let watchdogs = self.watchdogs.clone();
        let run = run.0.clone();
        tokio::task::spawn_blocking(move || {
            watchdogs
                .inspect()
                .ok()
                .and_then(|found| found.into_iter().find(|f| f.run == run))
        })
        .await
        .ok()
        .flatten()
    }

    async fn observe(&self, observation: Observed) {
        let claims = self.claims.clone();
        let _ = tokio::task::spawn_blocking(move || claims.observe(observation)).await;
    }

    async fn observe_found(&self, found: Found) {
        let claims = self.claims.clone();
        let generation = self.generation;
        let _ = tokio::task::spawn_blocking(move || {
            claims.observe_watchdog(&found, generation, nd_claims::BackendKind::Claude)
        })
        .await;
    }

    async fn refresh(&self) {
        let claims = self.claims.clone();
        let _ = tokio::task::spawn_blocking(move || claims.refresh()).await;
    }

    /// 等看守单元清理完，再把「已离开」报给独占登记；`kill` 时先把还活着的进程结束掉。
    /// 只有看守的观察证明进程不在了才放掉租约，看不清的不报 Gone。
    async fn settle_gone(&self, run: &RunId, kill: bool) -> bool {
        let deadline = tokio::time::Instant::now() + self.config.gone_timeout;
        let mut killed = false;
        loop {
            match self.inspect(run).await {
                None => {
                    self.observe(Observed::Gone {
                        run: run.0.clone(),
                        identity: None,
                        how: GoneHow::NeverLaunched,
                    })
                    .await;
                    return true;
                }
                Some(found) if found.state == "Gone" => {
                    self.observe_found(found).await;
                    return true;
                }
                Some(found) if found.state == "Up" && kill && !killed => {
                    self.observe_found(found).await;
                    if let Ok(mut link) = self.watchdogs.link(&run.0).await {
                        let _ = link.finish(Finish::Kill).await;
                    }
                    killed = true;
                }
                Some(_) => {}
            }
            if tokio::time::Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 扫描检查点之后的完整连续流水；读错或缺行不能当作“没写过”。
    async fn written(
        &self,
        run: &RunId,
        uuids: &HashSet<String>,
        after: u64,
    ) -> (HashMap<String, u64>, u64, bool) {
        let watchdogs = self.watchdogs.clone();
        let run = run.0.clone();
        let wanted = uuids.clone();
        tokio::task::spawn_blocking(move || {
            let mut found = HashMap::new();
            let mut through = after;
            let mut complete = true;
            loop {
                let records = match watchdogs.records(&run, through, 1000) {
                    Ok(records) => records,
                    Err(_) => {
                        complete = false;
                        break;
                    }
                };
                if records.is_empty() {
                    break;
                }
                for record in records {
                    if record.seq > through.saturating_add(1)
                        || matches!(
                            record.event,
                            Event::Gap {
                                reason: nd_watchdog_proto::GapReason::LostLines
                            }
                        )
                    {
                        complete = false;
                    }
                    through = record.end_seq;
                    if let Event::In { line, in_seq } = record.event
                        && let Ok(frame) = serde_json::from_str::<Value>(&line)
                        && let Some(uuid) = frame["uuid"]
                            .as_str()
                            .or_else(|| frame["request_id"].as_str())
                        && wanted.contains(uuid)
                    {
                        found.insert(uuid.to_owned(), in_seq);
                    }
                }
            }
            (found, through, complete)
        })
        .await
        .unwrap_or_else(|_| (HashMap::new(), after, false))
    }

    /// 先占住承载位的命令队列：接回进行中交来的票排在这里，任务起来后按序写出。
    fn reserve(&self, session: &SessionId, carrier: &CarrierId) -> mpsc::UnboundedReceiver<Cmd> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.carriers.lock().unwrap().insert(
            carrier.clone(),
            Slot {
                session: session.clone(),
                tx,
                normal_queued: Arc::new(AtomicUsize::new(0)),
            },
        );
        rx
    }

    fn start_actor(
        self: &Arc<Self>,
        session: SessionId,
        carrier: CarrierId,
        run: ClaudeRun,
        convo: Conversation,
        pending: HashMap<String, Ticket>,
    ) {
        let rx = self.reserve(&session, &carrier);
        self.spawn_actor(
            HashMap::new(),
            session,
            carrier,
            run,
            convo,
            pending,
            HashMap::new(),
            None,
            rx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_actor(
        self: &Arc<Self>,
        settings_controls: HashMap<String, Ticket>,
        session: SessionId,
        carrier: CarrierId,
        run: ClaudeRun,
        convo: Conversation,
        pending: HashMap<String, Ticket>,
        writes: HashMap<String, u64>,
        recover_through: Option<u64>,
        rx: mpsc::UnboundedReceiver<Cmd>,
    ) {
        self.spawn_actor_with_controls(
            HashMap::new(),
            settings_controls,
            BTreeMap::new(),
            session,
            carrier,
            run,
            convo,
            pending,
            writes,
            recover_through,
            rx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_actor_with_controls(
        self: &Arc<Self>,
        controls: HashMap<String, (Ticket, Act)>,
        settings_controls: HashMap<String, Ticket>,
        invokes: BTreeMap<String, PendingInvoke>,
        session: SessionId,
        carrier: CarrierId,
        run: ClaudeRun,
        convo: Conversation,
        pending: HashMap<String, Ticket>,
        writes: HashMap<String, u64>,
        recover_through: Option<u64>,
        rx: mpsc::UnboundedReceiver<Cmd>,
    ) {
        let me = self
            .carriers
            .lock()
            .unwrap()
            .get(&carrier)
            .map(|slot| slot.tx.clone())
            .expect("承载位的命令队列已先占住");
        let actor = Actor {
            inner: self.clone(),
            session,
            carrier,
            run_id: RunId(run.run().to_owned()),
            bs: run.ready().backend_session_id.clone(),
            run,
            convo,
            pending,
            controls,
            queued: VecDeque::new(),
            writes,
            ending: None,
            settings_controls,
            invokes,
            me,
            recover_through,
            rx,
        };
        self.runtime.spawn(actor.run());
    }

    async fn open(self: Arc<Self>, issued: Issued, carrier: CarrierId, run: RunId, spec: OpenSpec) {
        let bs = spec.origin.backend_session().clone();
        let start = match &spec.origin {
            Origin::Fresh { id } => Start::Fresh {
                session: id.id.clone(),
            },
            Origin::Resume { bs } => Start::Resume {
                session: bs.id.clone(),
            },
        };
        let open = Open {
            start,
            cwd: spec.profile.cwd.clone(),
            model: spec.profile.model.clone(),
            permission_mode: spec.profile.permission_mode.clone(),
        };
        match self.claude.open(&run.0, open, InitOptions::default()).await {
            Ok(mut claude_run) => {
                let mut restore = Vec::new();
                if let Some(effort) = spec.profile.effort {
                    restore.push(nd_wire::LiveSetting::Effort(effort));
                }
                restore.extend(spec.live_settings);
                for setting in restore {
                    if let Err(error) =
                        initial_control(&mut claude_run, setting_request(&setting)).await
                    {
                        self.settle_gone(&run, true).await;
                        self.deliver_facts(
                            &issued.session,
                            &carrier,
                            vec![done(&issued.ticket, Outcome::failed(error))],
                        )
                        .await;
                        return;
                    }
                }
                let mut settings =
                    initial_control(&mut claude_run, json!({"subtype":"get_settings"}))
                        .await
                        .map(settings_with_caps)
                        .unwrap_or_else(|why| json!({"caps":{},"error":why}));
                settings["models"] = claude_run.ready().initialize["models"].clone();
                settings["permission_mode"] =
                    claude_run.ready().initialize["current_permission_mode"].clone();
                let ready = claude_run.ready().clone();
                self.observe(Observed::Up {
                    run: run.0.clone(),
                    identity: ready.identity.clone(),
                    generation: self.generation,
                    kind: nd_claims::BackendKind::Claude,
                })
                .await;
                // 自己的进程写进 CLI 注册表的那一条按身份认作自有，不当外部写入者。
                self.refresh().await;
                let readiness = match &ready.caps.readiness {
                    ClaudeReadiness::Full => Readiness::Full,
                    ClaudeReadiness::ChatOnly { why } => Readiness::ChatOnly { why: why.clone() },
                };
                let mut adopt = serde_json::to_value(&ready.caps).unwrap_or(Value::Null);
                adopt["settings"] = settings;
                self.start_actor(
                    issued.session.clone(),
                    carrier.clone(),
                    claude_run,
                    Conversation::new(),
                    HashMap::new(),
                );
                // 新拉起的进程手上没有后台任务。
                self.deliver_facts(
                    &issued.session,
                    &carrier,
                    vec![
                        done(
                            &issued.ticket,
                            Outcome::Ok {
                                done: Done::Opened {
                                    bs,
                                    run: run.clone(),
                                    readiness,
                                    interaction: nd_backend::InteractionCaps {
                                        send_intents: vec![
                                            Intent::Fold,
                                            Intent::AfterTurn,
                                            Intent::Interrupting,
                                        ],
                                        withdraw: true,
                                        interrupt: true,
                                        cancel_queued: false,
                                        interrupt_spares_background: ready
                                            .caps
                                            .interrupt_spares_background,
                                        immediate_preserves_mcp: ![
                                            "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
                                            "CLAUDE_CODE_DISABLE_MCP_TASK_BACKGROUND",
                                        ]
                                        .iter()
                                        .any(|k| {
                                            self.claude
                                                .config()
                                                .env
                                                .get(*k)
                                                .is_some_and(|v| matches!(v.as_str(), "1" | "true"))
                                        }) && self
                                            .claude
                                            .config()
                                            .env
                                            .get("CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS")
                                            .is_none_or(|v| v != "0"),
                                        rewind_menu: true,
                                    },
                                    adopt,
                                    features: features(&ready.caps),
                                },
                            },
                        ),
                        fact(
                            format!("{run}:opened-tasks"),
                            FactBody::Tasks {
                                drain: Drain::Drained,
                            },
                        ),
                    ],
                )
                .await;
            }
            Err(error) => {
                // 拉起或握手失败：还活着的进程结束掉，等看守单元清理完再报失败，租约随之放掉。
                self.settle_gone(&run, true).await;
                self.claude.channel().unregister(&run.0);
                self.deliver_facts(
                    &issued.session,
                    &carrier,
                    vec![done(&issued.ticket, Outcome::failed(error.to_string()))],
                )
                .await;
            }
        }
    }

    /// 守护进程重启后接回：活着的进程续读流水，没写出的票证明没写出，已退出的报退出。
    async fn adopt(
        self: Arc<Self>,
        part: AdoptPart,
        mut queues: HashMap<CarrierId, mpsc::UnboundedReceiver<Cmd>>,
    ) {
        let mut handled: HashSet<Ticket> = HashSet::new();
        for record in &part.carriers {
            let mine: Vec<&PendingTicket> = part
                .pending
                .iter()
                .filter(|p| p.act.carrier() == &record.carrier)
                .filter(|p| !matches!(p.act, Act::Open { .. }))
                .collect();
            handled.extend(mine.iter().map(|p| p.issued.ticket.clone()));
            let Some(queued) = queues.remove(&record.carrier) else {
                continue;
            };
            self.clone()
                .adopt_carrier(&part.session, record, mine, queued)
                .await;
        }
        let mut recovered = HashSet::new();
        for pending in &part.pending {
            if handled.contains(&pending.issued.ticket) {
                continue;
            }
            let carrier = pending.act.carrier().clone();
            recovered.insert(carrier.clone());
            let outcome = match &pending.act {
                Act::Open { run, .. } => match self.inspect(run).await {
                    // 从没拉起过：证明没做，引擎重新放行后另发。
                    None => Outcome::Refused {
                        refusal: Refusal::Withheld,
                    },
                    Some(_) => {
                        self.settle_gone(run, true).await;
                        Outcome::failed("守护进程重启时拉起没有完成，已结束这个进程")
                    }
                },
                Act::End { .. } => Outcome::Ok {
                    done: Done::Ended { code: None },
                },
                Act::Interrupt { .. } | Act::Withdraw { .. } => {
                    Outcome::failed("控制目标进程不在了")
                }
                Act::Send { .. } | Act::Configure { .. } | Act::Invoke { .. } => Outcome::Refused {
                    refusal: Refusal::Withheld,
                },
            };
            self.deliver_facts(
                &part.session,
                &carrier,
                vec![done(&pending.issued.ticket, outcome)],
            )
            .await;
        }
        for carrier in recovered {
            self.deliver_facts(
                &part.session,
                &carrier,
                vec![fact("recovered".into(), FactBody::Recovered)],
            )
            .await;
        }
    }

    async fn adopt_carrier(
        self: Arc<Self>,
        session: &SessionId,
        record: &CarrierRecord,
        pending: Vec<&PendingTicket>,
        mut queued: mpsc::UnboundedReceiver<Cmd>,
    ) {
        let Some(run) = record.run.clone() else {
            return;
        };
        let uncertain_controls: HashSet<Ticket> = pending
            .iter()
            .filter(|p| p.unknown && matches!(p.act, Act::Interrupt { .. } | Act::Withdraw { .. }))
            .map(|p| p.issued.ticket.clone())
            .collect();
        let sends: HashMap<String, Ticket> = pending
            .iter()
            .filter(|p| matches!(p.act, Act::Send { .. }))
            .map(|p| (native_uuid(&p.issued.ticket), p.issued.ticket.clone()))
            .collect();
        let controls: HashMap<String, (Ticket, Act)> = pending
            .iter()
            .filter(|p| matches!(p.act, Act::Interrupt { .. } | Act::Withdraw { .. }))
            .map(|p| {
                (
                    native_uuid(&p.issued.ticket),
                    (p.issued.ticket.clone(), p.act.clone()),
                )
            })
            .collect();
        let mut written: HashMap<String, u64> = record
            .checkpoint
            .as_ref()
            .and_then(|c| serde_json::from_value(c.0["writes"].clone()).ok())
            .unwrap_or_default();
        let cursor = record
            .checkpoint
            .as_ref()
            .and_then(|c| c.0["seq"].as_u64())
            .unwrap_or(0);
        let known_prefix = record
            .checkpoint
            .as_ref()
            .is_none_or(|c| c.0["writes"].is_object());
        let (scanned, through, complete) = self
            .written(
                &run,
                &sends.keys().chain(controls.keys()).cloned().collect(),
                cursor,
            )
            .await;
        written.extend(scanned);
        let control_prefix = record
            .checkpoint
            .as_ref()
            .is_none_or(|c| c.0["controls"].is_object());
        if let Some(c) = &record.checkpoint
            && !c.0["writes"].is_object()
            && let Some(previous) = c.0["controls"].as_object()
        {
            for id in previous.keys() {
                written.entry(id.clone()).or_insert(0);
            }
        }
        // 交给动作 mod 的事：检查点里记着的（含没用掉的自动批准）加上引擎交来的未结票。
        let saved: BTreeMap<String, PendingInvoke> = record
            .checkpoint
            .as_ref()
            .and_then(|c| serde_json::from_value(c.0["invokes"].clone()).ok())
            .unwrap_or_default();
        let invokes: BTreeMap<String, PendingInvoke> = pending
            .iter()
            .filter_map(|p| match &p.act {
                Act::Invoke { invocation, .. } if !titles(invocation) => {
                    let op_id = native_uuid(&p.issued.ticket);
                    let approval = match saved.get(&op_id) {
                        Some(known) => known.approval.clone(),
                        None => match invocation {
                            Invocation::Shell { command } => Some(command.clone()),
                            _ => None,
                        },
                    };
                    Some((
                        op_id,
                        PendingInvoke {
                            ticket: p.issued.ticket.clone(),
                            invocation: invocation.clone(),
                            approval,
                        },
                    ))
                }
                _ => None,
            })
            .collect();
        let found = self.inspect(&run).await;
        let caps: Option<Caps> = serde_json::from_value(record.adopt.clone()).ok();
        let bs = record.bs.as_ref().map(|b| b.id.clone()).unwrap_or_default();
        let adopted = match (&found, caps.clone()) {
            (Some(f), Some(caps)) if f.state == "Up" => {
                self.claude.adopt(&run.0, &bs, caps).await.ok()
            }
            _ => None,
        };
        let mut facts = vec![];
        let Some(mut claude_run) = adopted else {
            // 看守已消失时不能从“不在剩余流水里”推导没写出。
            self.settle_gone(&run, true).await;
            for ticket in sends.values() {
                facts.push(done(
                    ticket,
                    Outcome::Unknown {
                        evidence: "后端已退出或接回失败，缺少确定的消费结论".into(),
                    },
                ));
            }
            for (ticket, _) in controls.values() {
                facts.push(done(
                    ticket,
                    Outcome::Unknown {
                        evidence: "控制来源已退出或无法接回，缺少确定的执行结论".into(),
                    },
                ));
            }
            for pending in invokes.values() {
                facts.push(done(
                    &pending.ticket,
                    Outcome::Unknown {
                        evidence: "后端已退出或接回失败，查不到动作 mod 的结论".into(),
                    },
                ));
            }
            for p in &pending {
                if matches!(&p.act, Act::Configure { .. })
                    || matches!(&p.act, Act::Invoke { invocation, .. } if titles(invocation))
                {
                    facts.push(done(
                        &p.issued.ticket,
                        Outcome::failed("设置时后端进程退出"),
                    ));
                }
                if let Act::End { .. } = p.act {
                    facts.push(done(
                        &p.issued.ticket,
                        Outcome::Ok {
                            done: Done::Ended { code: None },
                        },
                    ));
                }
            }
            // 接回期间交来的票：一行都没写，证明没写出。
            self.carriers.lock().unwrap().remove(&record.carrier);
            queued.close();
            while let Ok(cmd) = queued.try_recv() {
                match cmd {
                    Cmd::Send { ticket, .. }
                    | Cmd::Configure { ticket, .. }
                    | Cmd::Title { ticket, .. } => facts.push(done(
                        &ticket,
                        Outcome::Refused {
                            refusal: Refusal::Withheld,
                        },
                    )),
                    Cmd::End { ticket, .. } => facts.push(done(
                        &ticket,
                        Outcome::Ok {
                            done: Done::Ended { code: None },
                        },
                    )),
                    Cmd::Control { ticket, .. } => {
                        facts.push(done(&ticket, Outcome::failed("控制目标进程不在了")))
                    }
                    Cmd::Invoke { ticket, .. } => facts.push(done(
                        &ticket,
                        Outcome::Refused {
                            refusal: Refusal::Withheld,
                        },
                    )),
                    Cmd::Ack(_) | Cmd::Concluded { .. } => {}
                }
            }
            facts.push(fact(
                format!("exit:{run}"),
                FactBody::Exited {
                    run: run.clone(),
                    code: None,
                },
            ));
            facts.push(fact("recovered".into(), FactBody::Recovered));
            self.deliver_facts(session, &record.carrier, facts).await;
            return;
        };
        let (cursor, convo) = match &record.checkpoint {
            Some(Checkpoint(value)) => (
                value["seq"].as_u64().unwrap_or(0),
                serde_json::from_value(value["convo"].clone()).unwrap_or_default(),
            ),
            None => (0, Conversation::new()),
        };
        let recover_through = claude_run.cursor();
        claude_run.seek(cursor);
        if let Some(previous) = &caps
            && claude_run.ready().caps != *previous
        {
            // mod 没回来：接回的进程降为只能聊天，会话头要跟着变。
            facts.push(fact(
                format!("caps:{run}:{recover_through}"),
                FactBody::CapsChanged {
                    readiness: readiness_of(&claude_run.ready().caps),
                    features: features(&claude_run.ready().caps),
                },
            ));
        }
        let mut waiting = HashMap::new();
        for (uuid, ticket) in sends {
            if written.contains_key(&uuid) {
                // 写过了：从检查点接着读，等它的回显。
                waiting.insert(uuid, ticket);
            } else if known_prefix && complete && through >= recover_through {
                facts.push(done(
                    &ticket,
                    Outcome::Refused {
                        refusal: Refusal::Withheld,
                    },
                ));
                // 若原票已经 Unknown，只更新证实未送达的消息；不会自动另发。
                facts.push(clarified(
                    &ticket,
                    Outcome::Refused {
                        refusal: Refusal::Lost {
                            evidence: "完整检查点与连续流水证明原输入未写出".into(),
                        },
                    },
                ));
            } else {
                facts.push(done(
                    &ticket,
                    Outcome::Unknown {
                        evidence: "检查点或流水不完整，不能证明原输入未写出".into(),
                    },
                ));
                waiting.insert(uuid, ticket);
            }
        }
        let recovered_controls = controls
            .iter()
            .filter(|(id, _)| written.contains_key(*id))
            .map(|(id, value)| (id.clone(), value.clone()))
            .collect();
        let unsent_controls: Vec<_> = controls
            .into_iter()
            .filter(|(id, _)| !written.contains_key(id))
            .map(|(_, value)| value)
            .collect();
        let mut restored_controls = HashMap::new();
        for p in &pending {
            if let Act::Invoke { invocation, .. } = &p.act
                && titles(invocation)
            {
                let id = format!("title:{}", p.issued.ticket);
                let (seen, _, complete) = self.written(&run, &HashSet::from([id.clone()]), 0).await;
                let was_written = seen.contains_key(&id)
                    || record.checkpoint.as_ref().is_some_and(|c| {
                        !c.0["settings_controls"][&id].is_null() || !c.0["controls"][&id].is_null()
                    });
                if was_written {
                    restored_controls.insert(id, p.issued.ticket.clone());
                } else if !complete {
                    facts.push(done(
                        &p.issued.ticket,
                        Outcome::Unknown {
                            evidence: "标题流水不完整，不能重发生成请求".into(),
                        },
                    ));
                } else if let Some(slot) = self.carriers.lock().unwrap().get(&record.carrier) {
                    let _ = slot.tx.send(Cmd::Title {
                        ticket: p.issued.ticket.clone(),
                        invocation: invocation.clone(),
                        write: true,
                    });
                }
            }
            if let Act::Configure { setting, .. } = &p.act
                && let Some(slot) = self.carriers.lock().unwrap().get(&record.carrier)
            {
                let _ = slot.tx.send(Cmd::Configure {
                    ticket: p.issued.ticket.clone(),
                    setting: setting.clone(),
                });
            }
            if let Act::End { how, .. } = &p.act {
                // 结束可以重发。
                let slot = self.carriers.lock().unwrap();
                if let Some(slot) = slot.get(&record.carrier) {
                    let _ = slot.tx.send(Cmd::End {
                        ticket: p.issued.ticket.clone(),
                        how: *how,
                    });
                }
            }
        }
        let rechecks: Vec<String> = invokes.keys().cloned().collect();
        self.spawn_actor_with_controls(
            recovered_controls,
            restored_controls,
            invokes,
            session.clone(),
            record.carrier.clone(),
            claude_run,
            convo,
            waiting,
            written,
            Some(recover_through),
            queued,
        );
        if !facts.is_empty() {
            self.deliver_facts(session, &record.carrier, facts).await;
        }
        let me = self
            .carriers
            .lock()
            .unwrap()
            .get(&record.carrier)
            .map(|slot| slot.tx.clone());
        if let Some(me) = me {
            for op_id in rechecks {
                self.runtime.spawn(recheck(
                    self.claude.channel().clone(),
                    run.0.clone(),
                    op_id,
                    me.clone(),
                ));
            }
        }
        for (ticket, act) in unsent_controls {
            if uncertain_controls.contains(&ticket) {
                if control_prefix && complete && through >= recover_through {
                    self.deliver_facts(
                        session,
                        &record.carrier,
                        vec![clarified(
                            &ticket,
                            Outcome::Refused {
                                refusal: Refusal::Lost {
                                    evidence: "连续流水和检查点证明控制请求未写出".into(),
                                },
                            },
                        )],
                    )
                    .await;
                }
                continue;
            }
            if control_prefix && complete && through >= recover_through {
                if let Some(slot) = self.carriers.lock().unwrap().get(&record.carrier) {
                    let _ = slot.tx.send(Cmd::Control { ticket, act });
                }
            } else {
                self.deliver_facts(
                    session,
                    &record.carrier,
                    vec![done(
                        &ticket,
                        Outcome::Unknown {
                            evidence: "控制请求的检查点或流水不完整，不能证明未写出".into(),
                        },
                    )],
                )
                .await;
            }
        }
    }
}

struct Actor {
    inner: Arc<Inner>,
    session: SessionId,
    carrier: CarrierId,
    run_id: RunId,
    bs: String,
    run: ClaudeRun,
    convo: Conversation,
    /// 写出了、还没见到回显的 user 行：uuid → 票。
    pending: HashMap<String, Ticket>,
    controls: HashMap<String, (Ticket, Act)>,
    queued: VecDeque<Cmd>,
    writes: HashMap<String, u64>,
    ending: Option<Ticket>,
    settings_controls: HashMap<String, Ticket>,
    /// 交给动作 mod、还没有结论的事：操作 id → 票。
    invokes: BTreeMap<String, PendingInvoke>,
    /// 自己的命令队列（等动作 mod 结论的任务从这里交回来）。
    me: mpsc::UnboundedSender<Cmd>,
    recover_through: Option<u64>,
    rx: mpsc::UnboundedReceiver<Cmd>,
}

impl Actor {
    async fn write_control(&mut self, frame: &Value) -> Result<(), String> {
        let seq = self.run.next_input();
        match self.run.write(frame).await {
            Ok(_) => Ok(()),
            Err(error) => {
                if !self.relink().await {
                    return Err(error.to_string());
                }
                if self.run.next_input() > seq {
                    return Ok(());
                }
                self.run
                    .write(frame)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }
    async fn run(mut self) {
        if let Some(through) = self.recover_through.take() {
            while self.run.cursor() < through {
                if self.poll().await {
                    break;
                }
                if self.run.cursor() < through {
                    tokio::time::sleep(self.inner.config.poll).await;
                }
            }
            self.inner
                .deliver_facts(
                    &self.session,
                    &self.carrier,
                    vec![fact("recovered".into(), FactBody::Recovered)],
                )
                .await;
        }
        let mut poll = tokio::time::interval(self.inner.config.poll);
        loop {
            if self.queued.is_empty() {
                tokio::select! {
                    biased;
                    _ = poll.tick() => { if self.poll().await { return; } },
                    cmd = self.rx.recv() => match cmd { Some(cmd) => self.queued.push_back(cmd), None => return },
                }
            }
            while let Ok(cmd) = self.rx.try_recv() {
                self.queued.push_back(cmd);
            }
            if !self.queued.is_empty() {
                let control = self
                    .queued
                    .iter()
                    .position(|cmd| !matches!(cmd, Cmd::Send { .. } | Cmd::Invoke { .. }))
                    .unwrap_or(0);
                // 撤回依赖对应输入已经写出；取消整队依赖此前输入。普通 Esc 无此依赖，优先写。
                let prerequisite = match &self.queued[control] {
                    Cmd::Control {
                        act: Act::Withdraw { send, .. },
                        ..
                    } => self
                        .queued
                        .iter()
                        .position(|cmd| matches!(cmd,Cmd::Send { ticket, .. } if ticket == send)),
                    Cmd::Control {
                        act:
                            Act::Interrupt {
                                queued: nd_backend::QueuedPolicy::Cancel,
                                ..
                            },
                        ..
                    } => self
                        .queued
                        .iter()
                        .take(control)
                        .position(|cmd| matches!(cmd, Cmd::Send { .. })),
                    _ => None,
                };
                let cmd = self.queued.remove(prerequisite.unwrap_or(control)).unwrap();
                self.command(cmd).await;
                // 长输入队列也持续读回应，不能让事实或控制结果饿死。
                if self.poll().await {
                    return;
                }
            }
        }
    }

    async fn relink(&mut self) -> bool {
        match self.inner.watchdogs.link(&self.run_id.0).await {
            Ok(link) => {
                self.run.relink(link);
                true
            }
            Err(_) => false,
        }
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Send { ticket, msg, .. } => {
                let uuid = native_uuid(&ticket);
                let priority = match msg.intent {
                    Intent::Fold => "next",
                    Intent::AfterTurn => "later",
                    Intent::Interrupting => "now",
                };
                let blobs = self.inner.blobs.clone();
                let message = msg.clone();
                let content = tokio::task::spawn_blocking(move || -> Result<Vec<Value>, String> {
                    use base64::Engine;
                    let mut content = vec![];
                    if !message.text.is_empty() { content.push(json!({"type":"text","text":message.text})); }
                    for attachment in &message.attachments {
                        let bytes = blobs.get(&attachment.blob).map_err(|e| e.to_string())?;
                        if attachment.media_type == "text/plain" {
                            let text = String::from_utf8(bytes).map_err(|_| format!("{} 不是 UTF-8 文本", attachment.name))?;
                            content.push(json!({"type":"text","text":format!("附件 {}：\n{}", serde_json::to_string(&attachment.name).unwrap(), text)}));
                        } else {
                            let kind = if attachment.media_type == "application/pdf" { "document" } else { "image" };
                            content.push(json!({"type":kind,"source":{"type":"base64","media_type":attachment.media_type,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}}));
                        }
                    }
                    Ok(content)
                }).await;
                let content = match content {
                    Ok(Ok(content)) => content,
                    error => {
                        self.inner
                            .deliver_facts(
                                &self.session,
                                &self.carrier,
                                vec![done(
                                    &ticket,
                                    Outcome::failed(format!("读取附件失败：{error:?}")),
                                )],
                            )
                            .await;
                        return;
                    }
                };
                let frame = json!({
                    "type": "user",
                    "uuid": uuid,
                    "session_id": self.bs,
                    "parent_tool_use_id": null,
                    "message": {"role": "user", "content": content},
                    "priority": priority,
                    "origin": {"kind": "human"},
                });
                // 在写出之前判定确定的超限；不能把一个从未写出的附件误标为交付不明。
                if frame.to_string().len() > nd_watchdog_proto::MAX_FRAME / 4 {
                    self.inner
                        .deliver_facts(
                            &self.session,
                            &self.carrier,
                            vec![done(
                                &ticket,
                                Outcome::failed("消息编码后过大，请减少附件或正文"),
                            )],
                        )
                        .await;
                    return;
                }
                self.pending.insert(uuid.clone(), ticket.clone());
                #[cfg(feature = "scenarios")]
                self.stop_fault(&msg.text);
                #[cfg(feature = "scenarios")]
                if self.delivery_fault(&msg.text, false).is_some() {
                    self.inner
                        .deliver_facts(
                            &self.session,
                            &self.carrier,
                            vec![done(
                                &ticket,
                                Outcome::Unknown {
                                    evidence: "场景故障：输入传输回应丢失，尚未核实写入".into(),
                                },
                            )],
                        )
                        .await;
                    return;
                }
                if let Err(error) = self.run.write(&frame).await {
                    // 传输出错（例如别处接管了看守连接）：换连接后按看守报的已写高水位定，
                    // 没写过就同一行再写一次（传输层重发，uuid 与输入序号都不变）。
                    let mut failure = Some(error.to_string());
                    let seq = self.run.next_input();
                    if self.relink().await {
                        // 新连接报的已写高水位越过了这一行的序号：写过了。否则用同一个序号再写，
                        // 旧连接上已受理的那次若也落了地，看守按序号去重。
                        failure = if self.run.next_input() > seq {
                            None
                        } else {
                            self.run.write(&frame).await.err().map(|e| e.to_string())
                        };
                    }
                    if let Some(error) = failure {
                        // 保留原票映射；之后出现原 uuid 的回显或拒绝时上报 Clarified。
                        self.inner
                            .deliver_facts(
                                &self.session,
                                &self.carrier,
                                vec![done(
                                    &ticket,
                                    Outcome::Unknown {
                                        evidence: format!("写给看守时连接出错：{error}"),
                                    },
                                )],
                            )
                            .await;
                    }
                }
                #[cfg(feature = "scenarios")]
                if let Some(action) = self.delivery_fault(&msg.text, true) {
                    if action == "crash_after_write" {
                        std::process::abort();
                    }
                    if action == "pause_after_write" {
                        let _ = rustix::process::kill_process(
                            rustix::process::Pid::from_raw(std::process::id() as i32).unwrap(),
                            rustix::process::Signal::STOP,
                        );
                    }
                    if action == "unknown_after_write" {
                        self.inner
                            .deliver_facts(
                                &self.session,
                                &self.carrier,
                                vec![done(
                                    &ticket,
                                    Outcome::Unknown {
                                        evidence: "场景故障：写出后连接中断".into(),
                                    },
                                )],
                            )
                            .await;
                    }
                }
            }
            Cmd::Control { ticket, act } => {
                let id = native_uuid(&ticket);
                let request = match &act {
                    Act::Interrupt { queued, .. } => {
                        if *queued == nd_backend::QueuedPolicy::Cancel {
                            if !self.convo.can_cancel_queued() {
                                self.inner
                                    .deliver_facts(
                                        &self.session,
                                        &self.carrier,
                                        vec![done(
                                            &ticket,
                                            Outcome::Rejected {
                                                reject: Reject::Unsupported {
                                                    why: "后端未声明取消排队能力".into(),
                                                },
                                            },
                                        )],
                                    )
                                    .await;
                                return;
                            }
                            json!({"subtype":"interrupt","cancel_queued":true})
                        } else {
                            json!({"subtype":"interrupt"})
                        }
                    }
                    Act::Withdraw { send, .. } => {
                        json!({"subtype":"cancel_async_message", "message_uuid":native_uuid(send)})
                    }
                    _ => unreachable!(),
                };
                let frame = json!({"type":"control_request", "request_id":id, "request":request});
                #[cfg(feature = "scenarios")]
                self.stop_fault(request["subtype"].as_str().unwrap_or_default());
                self.controls.insert(id.clone(), (ticket.clone(), act));
                #[cfg(feature = "scenarios")]
                if self
                    .delivery_fault(request["subtype"].as_str().unwrap_or_default(), false)
                    .is_some()
                {
                    self.inner
                        .deliver_facts(
                            &self.session,
                            &self.carrier,
                            vec![done(
                                &ticket,
                                Outcome::Unknown {
                                    evidence: "场景故障：控制请求传输未确认".into(),
                                },
                            )],
                        )
                        .await;
                    return;
                }
                // 看守按实际输入序号去重；不把连接失败当作请求未送达。
                let seq = self.run.next_input();
                if let Err(error) = self.run.write(&frame).await {
                    let recovered = self.relink().await
                        && (self.run.next_input() > seq || self.run.write(&frame).await.is_ok());
                    if !recovered {
                        // 保留请求配对，迟到回应仍可澄清原 Unknown；writes 才是写出证据。
                        self.inner
                            .deliver_facts(
                                &self.session,
                                &self.carrier,
                                vec![done(
                                    &ticket,
                                    Outcome::Unknown {
                                        evidence: error.to_string(),
                                    },
                                )],
                            )
                            .await;
                    }
                }
            }
            Cmd::Title {
                ticket,
                invocation,
                write,
            } => {
                let id = format!("title:{ticket}");
                self.settings_controls.insert(id.clone(), ticket.clone());
                let request = match invocation {
                    nd_backend::Invocation::Title { title } => {
                        json!({"subtype":"rename_session","title":title,"source":"host"})
                    }
                    nd_backend::Invocation::GenerateTitle { description } => {
                        json!({"subtype":"generate_session_title","description":description,"persist":true})
                    }
                    // 其余 Invoke 经动作 mod（`Cmd::Invoke`），不会排进这里。
                    _ => return,
                };
                if write
                    && let Err(error) = self
                        .write_control(
                            &json!({"type":"control_request","request_id":id,"request":request}),
                        )
                        .await
                {
                    self.settings_controls.remove(&id);
                    self.inner
                        .deliver_facts(
                            &self.session,
                            &self.carrier,
                            vec![done(
                                &ticket,
                                Outcome::Unknown {
                                    evidence: error.to_string(),
                                },
                            )],
                        )
                        .await;
                }
            }
            Cmd::Configure { ticket, setting } => {
                let request = setting_request(&setting);
                let id = format!("configure:{ticket}");
                self.settings_controls.insert(id.clone(), ticket.clone());
                if let Err(error) = self
                    .write_control(
                        &json!({"type":"control_request","request_id":id,"request":request}),
                    )
                    .await
                {
                    self.settings_controls.remove(&id);
                    self.inner
                        .deliver_facts(
                            &self.session,
                            &self.carrier,
                            vec![done(
                                &ticket,
                                Outcome::Unknown {
                                    evidence: error.to_string(),
                                },
                            )],
                        )
                        .await;
                }
            }
            Cmd::End { ticket, how } => {
                self.ending = Some(ticket);
                let result = match how {
                    EndHow::Graceful | EndHow::Finish => {
                        let frame = json!({
                            "type": "control_request",
                            "request_id": format!("nd-end-{}", self.run_id),
                            "request": {"subtype": "end_session"},
                        });
                        self.run.write(&frame).await.map(|_| ())
                    }
                    EndHow::Kill | EndHow::Discard => self.run.finish(Finish::Kill).await,
                };
                if result.is_err() {
                    self.relink().await;
                    let _ = self.run.finish(Finish::Kill).await;
                }
            }
            Cmd::Invoke { ticket, invocation } => self.invoke(ticket, invocation).await,
            Cmd::Concluded { op_id, result } => {
                let Some(pending) = self.invokes.remove(&op_id) else {
                    return;
                };
                let outcome = invoke::outcome(&pending.invocation, result.as_ref());
                let through = self.run.cursor();
                self.deliver_checkpointed(vec![done(&pending.ticket, outcome)], through)
                    .await;
            }
            Cmd::Ack(seq) => {
                if self.run.ack(seq).await.is_err() {
                    self.relink().await;
                }
            }
        }
    }
    async fn encode(&self, msg: &Msg) -> Result<Vec<Value>, String> {
        let blobs = self.inner.blobs.clone();
        let message = msg.clone();
        tokio::task::spawn_blocking(move || encode(&blobs, &message))
            .await
            .map_err(|e| e.to_string())?
    }

    fn checkpoint(&self, through: u64) -> Checkpoint {
        Checkpoint(json!({
            "seq": through,
            "convo": self.convo,
            "writes": self.writes,
            "controls": self.controls,
            "settings_controls": self.settings_controls,
            "invokes": self.invokes,
        }))
    }

    /// 交一批事实，带上当前的检查点（流水位置不变时也要记下未结的动作 mod 命令）。
    async fn deliver_checkpointed(&self, facts: Vec<Fact>, through: u64) {
        self.inner
            .deliver(
                &self.session,
                Batch {
                    carrier: self.carrier.clone(),
                    facts,
                    live: vec![],
                    checkpoint: Some(self.checkpoint(through)),
                },
            )
            .await;
    }

    /// 总结、`!`、派 fork 型子代理：经动作 mod 做，按票派生的操作 id 等结论。
    async fn invoke(&mut self, ticket: Ticket, invocation: Invocation) {
        let feature = match &invocation {
            Invocation::Compact { .. } => Feature::Summarize,
            Invocation::Shell { .. } => Feature::BangMode,
            Invocation::ForkAgent { .. } => Feature::ForkSubagent,
            Invocation::Title { .. } | Invocation::GenerateTitle { .. } => return,
        };
        let reject = |why: String| {
            done(
                &ticket,
                Outcome::Rejected {
                    reject: Reject::Unsupported { why },
                },
            )
        };
        if let Some(why) = self.run.ready().caps.unsupported(feature) {
            self.inner
                .deliver_facts(&self.session, &self.carrier, vec![reject(why)])
                .await;
            return;
        }
        let row = match &invocation {
            Invocation::Compact { anchor, .. } => {
                let msg = Msg {
                    attachments: anchor.attachments.clone(),
                    text: anchor.text.clone(),
                    intent: Intent::Fold,
                };
                match self.encode(&msg).await {
                    Ok(content) => Some(invoke::row_text(&content)),
                    Err(error) => {
                        self.inner
                            .deliver_facts(
                                &self.session,
                                &self.carrier,
                                vec![done(
                                    &ticket,
                                    Outcome::failed(format!("读取所选提示的附件失败：{error}")),
                                )],
                            )
                            .await;
                        return;
                    }
                }
            }
            _ => None,
        };
        let op_id = native_uuid(&ticket);
        let Some(action) = invoke::action(&invocation, row.as_deref()) else {
            return;
        };
        let approval = match &invocation {
            Invocation::Shell { command } => Some(command.clone()),
            _ => None,
        };
        self.invokes.insert(
            op_id.clone(),
            PendingInvoke {
                ticket: ticket.clone(),
                invocation,
                approval,
            },
        );
        if !self.run.send_as(ModName::Actions, &op_id, action) {
            self.invokes.remove(&op_id);
            self.inner
                .deliver_facts(
                    &self.session,
                    &self.carrier,
                    vec![done(
                        &ticket,
                        Outcome::Refused {
                            refusal: Refusal::Other {
                                why: "动作 mod 没有报到这个后端会话，命令没有发出".into(),
                            },
                        },
                    )],
                )
                .await;
            return;
        }
        let channel = self.inner.claude.channel().clone();
        let run = self.run_id.0.clone();
        let me = self.me.clone();
        let op = op_id.clone();
        self.inner.runtime.spawn(async move {
            let result = channel.result(&run, &op, INVOKE_WAIT).await;
            let _ = me.send(Cmd::Concluded { op_id: op, result });
        });
        let through = self.run.cursor();
        self.deliver_checkpointed(
            vec![fact(
                format!("invoke-sent:{op_id}"),
                FactBody::Written {
                    ticket,
                    native: op_id,
                },
            )],
            through,
        )
        .await;
    }

    #[cfg(feature = "scenarios")]
    fn delivery_fault(&self, text: &str, written: bool) -> Option<String> {
        let path = self
            .inner
            .claude
            .config()
            .socket
            .with_file_name("delivery-fault.json");
        let value: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        if !value["contains"].as_str().is_some_and(|s| text.contains(s)) {
            return None;
        }
        if (value["action"] == "unknown_without_write") == written {
            return None;
        }
        std::fs::remove_file(path).ok()?;
        value["action"].as_str().map(str::to_owned)
    }

    /// 场景构建的故障点：写某条消息之前让后端进程停住（SIGSTOP），模拟「写出了、还没被处理」。
    /// 故障文件在 mod 通道 socket 旁边，用一次就删掉；生产构建没有这段。
    #[cfg(feature = "scenarios")]
    fn stop_fault(&self, text: &str) {
        let path = self
            .inner
            .claude
            .config()
            .socket
            .with_file_name("backend-fault.json");
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let Ok(fault) = serde_json::from_slice::<Value>(&bytes) else {
            return;
        };
        if fault["contains"].as_str().is_some_and(|m| text.contains(m)) {
            let _ = std::fs::remove_file(&path);
            if let Some(pid) = rustix::process::Pid::from_raw(self.run.ready().identity.pid as i32)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::STOP);
            }
        }
    }

    /// 读一页流水，交一批。返回 true 表示进程已退出、这个任务结束。
    async fn poll(&mut self) -> bool {
        let records = match self.run.read(1000).await {
            Ok(records) => records,
            Err(_) => {
                if self.relink().await {
                    return false;
                }
                // 看守也不在了：流水还在磁盘上，读完剩下的；看守单元确实没了才按退出（码不明）收。
                let rest = self
                    .inner
                    .watchdogs
                    .records(&self.run_id.0, self.run.cursor(), 1000)
                    .unwrap_or_default();
                if !rest.is_empty() {
                    self.run.seek(rest.last().unwrap().end_seq);
                    return self.process(rest, None).await;
                }
                match self.inner.inspect(&self.run_id).await {
                    Some(found) if found.state != "Gone" => return false,
                    _ => return self.process(vec![], Some(-1)).await,
                }
            }
        };
        self.process(records, None).await
    }

    fn record(&self, records: &[nd_watchdog_proto::Record]) {
        let Some(dir) = &self.inner.config.record_dir else {
            return;
        };
        use std::io::Write;
        let _ = std::fs::create_dir_all(dir);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{}.jsonl", self.run_id)))
        {
            for record in records {
                if let Ok(line) = serde_json::to_string(record) {
                    let _ = writeln!(file, "{line}");
                }
            }
        }
    }

    async fn process(
        &mut self,
        records: Vec<nd_watchdog_proto::Record>,
        forced_exit: Option<i32>,
    ) -> bool {
        if records.is_empty() && forced_exit.is_none() {
            return false;
        }
        self.record(&records);
        let through = records.last().map_or(self.run.cursor(), |r| r.end_seq);
        let mut facts = vec![];
        let mut live = vec![];
        let mut exited = forced_exit;
        for record in &records {
            let had_cancel = self.convo.can_cancel_queued();
            if let Event::In { in_seq, line } = &record.event
                && let Ok(frame) = serde_json::from_str::<Value>(line)
                && let Some(uuid) = frame["uuid"]
                    .as_str()
                    .or_else(|| frame["request_id"].as_str())
                && (self.pending.contains_key(uuid) || self.controls.contains_key(uuid))
            {
                self.writes.insert(uuid.to_owned(), *in_seq);
            }
            for (n, convo) in self.convo.apply(record).into_iter().enumerate() {
                let key = format!("{}:{}:{n}", self.run_id, record.seq);
                match convo {
                    Convo::TitleChanged { title } => {
                        facts.push(fact(key, FactBody::TitleChanged { title }))
                    }
                    Convo::Written { uuid } => {
                        if let Some(ticket) = self.pending.get(&uuid) {
                            facts.push(fact(
                                key,
                                FactBody::Written {
                                    ticket: ticket.clone(),
                                    native: uuid,
                                },
                            ));
                        }
                    }
                    Convo::Echo { uuid } => {
                        self.writes.remove(&uuid);
                        if let Some(ticket) = self.pending.remove(&uuid) {
                            let outcome = Outcome::Ok {
                                done: Done::Landed { native: uuid },
                            };
                            facts.push(done(&ticket, outcome.clone()));
                            facts.push(clarified(&ticket, outcome));
                        }
                    }
                    Convo::Lifecycle { uuid, state }
                        if matches!(state.as_str(), "refused" | "discarded") =>
                    {
                        if let Some(ticket) = self.pending.remove(&uuid) {
                            self.writes.remove(&uuid);
                            let outcome = Outcome::Refused {
                                refusal: Refusal::Lost {
                                    evidence: format!("CLI 明确没有处理原 uuid 的消息（{state}）"),
                                },
                            };
                            facts.push(done(&ticket, outcome.clone()));
                            facts.push(clarified(&ticket, outcome));
                        }
                    }
                    Convo::Reply {
                        request_id,
                        ok,
                        body,
                    } => {
                        if let Some((ticket, act)) = self.controls.remove(&request_id) {
                            self.writes.remove(&request_id);
                            let outcome = if !ok {
                                Outcome::failed(body.to_string())
                            } else {
                                match act {
                                    Act::Interrupt { queued, .. } => {
                                        if queued == nd_backend::QueuedPolicy::Cancel
                                            && !body["response"]["cancelled"].is_array()
                                        {
                                            Outcome::Unknown {
                                                evidence:
                                                    "中断回应缺少 cancelled，未将排队消息当成已撤回"
                                                        .into(),
                                            }
                                        } else {
                                            let cancelled = body["response"]["cancelled"]
                                                .as_array()
                                                .into_iter()
                                                .flatten()
                                                .filter_map(|id| {
                                                    id.as_str().and_then(|id| {
                                                        self.writes.remove(id);
                                                        self.pending.remove(id)
                                                    })
                                                })
                                                .collect();
                                            Outcome::Ok {
                                                done: Done::Interrupted { cancelled },
                                            }
                                        }
                                    }
                                    Act::Withdraw { send, .. } => {
                                        match body["response"]["cancelled"].as_bool() {
                                            Some(cancelled) => {
                                                if cancelled {
                                                    let uuid = native_uuid(&send);
                                                    self.writes.remove(&uuid);
                                                    self.pending.remove(&uuid);
                                                }
                                                Outcome::Ok {
                                                    done: Done::Withdrawn { ok: cancelled },
                                                }
                                            }
                                            None => Outcome::Unknown {
                                                evidence: "撤回回应缺少 cancelled".into(),
                                            },
                                        }
                                    }
                                    _ => unreachable!(),
                                }
                            };
                            facts.push(done(&ticket, outcome.clone()));
                            facts.push(clarified(&ticket, outcome));
                        }
                        if let Some(ticket) = self.settings_controls.remove(&request_id) {
                            if !ok {
                                let reason = body["error"]
                                    .as_str()
                                    .unwrap_or("CLI 拒绝此设置")
                                    .to_owned();
                                let outcome = if request_id.starts_with("settings:") {
                                    Outcome::Unknown {
                                        evidence: format!("设置已被接受，但回读失败：{reason}"),
                                    }
                                } else {
                                    Outcome::failed(reason)
                                };
                                facts.push(done(&ticket, outcome));
                            } else if request_id.starts_with("title:") {
                                facts.push(done(
                                    &ticket,
                                    Outcome::Ok {
                                        done: Done::Titled {
                                            title: body["response"]["title"]
                                                .as_str()
                                                .map(str::to_owned),
                                        },
                                    },
                                ));
                            } else if request_id.starts_with("configure:") {
                                let id = format!("settings:{ticket}");
                                self.settings_controls.insert(id.clone(), ticket.clone());
                                if let Err(error) = self.write_control(&json!({"type":"control_request","request_id":id,"request":{"subtype":"get_settings"}})).await {
                                    self.settings_controls.remove(&id);
                                    facts.push(done(&ticket, Outcome::Unknown {evidence:error.to_string()}));
                                }
                            } else {
                                facts.push(done(
                                    &ticket,
                                    Outcome::Ok {
                                        done: Done::Configured {
                                            settings: settings_with_caps(body["response"].clone()),
                                        },
                                    },
                                ));
                            }
                        }
                    }
                    Convo::Lifecycle { .. } => {}
                    Convo::TurnMapped {
                        turn,
                        uuids,
                        complete,
                        last_assistant,
                    } => facts.push(fact(
                        key,
                        FactBody::TurnMapped {
                            turn: format!("{}:{turn}", self.run_id),
                            natives: uuids,
                            complete,
                            last_assistant,
                        },
                    )),
                    Convo::TurnStarted => facts.push(fact(key, FactBody::TurnStarted)),
                    Convo::TurnEnded {
                        ok, subtype, error, ..
                    } => facts.push(fact(key, FactBody::TurnEnded { ok, subtype, error })),
                    Convo::Delta { item, kind, text } => {
                        live.push(Live::Delta { item, kind, text })
                    }
                    Convo::Block { item } => facts.push(fact(key, FactBody::Item { item })),
                    Convo::Asked {
                        request_id,
                        subtype,
                        raw,
                    } => {
                        let mine = self.invokes.values_mut().find(|p| {
                            p.approval
                                .as_deref()
                                .is_some_and(|command| invoke::auto_approves(command, &raw))
                        });
                        if let Some(pending) = mine {
                            // `!` 的自动批准：只这一次，用掉就清。
                            pending.approval = None;
                            let answer = invoke::allow(&request_id, &raw);
                            let _ = self.write_control(&answer).await;
                            continue;
                        }
                        facts.push(fact(
                            key,
                            FactBody::Asked {
                                id: request_id,
                                kind: subtype,
                                raw,
                            },
                        ))
                    }
                    Convo::Tasks { drain } => facts.push(fact(key, FactBody::Tasks { drain })),
                    Convo::Gap { lost } => facts.push(fact(key, FactBody::Gap { lost })),
                    Convo::Exit { code } => exited = Some(code),
                }
            }
            if had_cancel != self.convo.can_cancel_queued() {
                facts.push(fact(
                    format!("{}:{}:cancel-queue-cap", self.run_id, record.seq),
                    FactBody::CanCancelQueued {
                        available: self.convo.can_cancel_queued(),
                    },
                ));
            }
        }
        if let Some(code) = exited {
            for (_, (ticket, _)) in self.controls.drain() {
                facts.push(done(
                    &ticket,
                    Outcome::Unknown {
                        evidence: "控制请求写出后进程退出，未见回应".into(),
                    },
                ));
            }
            for (_, ticket) in self.settings_controls.drain() {
                facts.push(done(
                    &ticket,
                    Outcome::Unknown {
                        evidence: "后端进程退出，设置或标题结果未确认".into(),
                    },
                ));
            }
            for (_, ticket) in self.pending.drain() {
                facts.push(done(
                    &ticket,
                    Outcome::Unknown {
                        evidence: "写出后进程退出，没有等到回显".into(),
                    },
                ));
            }
            for (_, pending) in std::mem::take(&mut self.invokes) {
                facts.push(done(
                    &pending.ticket,
                    Outcome::Unknown {
                        evidence: "交给动作 mod 之后进程退出，没有等到结论".into(),
                    },
                ));
            }
            if let Some(ticket) = self.ending.take() {
                facts.push(done(
                    &ticket,
                    Outcome::Ok {
                        done: Done::Ended { code: Some(code) },
                    },
                ));
            }
            // 等看守单元清理完、独占登记记下它离开，才报退出：之后的按需拉起不会撞上旧租约。
            self.inner.settle_gone(&self.run_id, false).await;
            self.inner.claude.channel().unregister(&self.run_id.0);
            facts.push(fact(
                format!("exit:{}", self.run_id),
                FactBody::Exited {
                    run: self.run_id.clone(),
                    code: Some(code),
                },
            ));
            self.inner.carriers.lock().unwrap().remove(&self.carrier);
            // 进程退出时还排在队列里、没写出的票：证明没写出，引擎另发（会按需拉起）。
            self.rx.close();
            while let Some(cmd) = self.queued.pop_front().or_else(|| self.rx.try_recv().ok()) {
                match cmd {
                    Cmd::Send { ticket, .. }
                    | Cmd::Configure { ticket, .. }
                    | Cmd::Title { ticket, .. } => facts.push(done(
                        &ticket,
                        Outcome::Refused {
                            refusal: Refusal::Withheld,
                        },
                    )),
                    Cmd::End { ticket, .. } => facts.push(done(
                        &ticket,
                        Outcome::Ok {
                            done: Done::Ended { code: Some(code) },
                        },
                    )),
                    Cmd::Control { ticket, .. } => {
                        facts.push(done(&ticket, Outcome::failed("控制目标进程不在了")))
                    }
                    Cmd::Invoke { ticket, .. } => facts.push(done(
                        &ticket,
                        Outcome::Refused {
                            refusal: Refusal::Withheld,
                        },
                    )),
                    Cmd::Ack(_) | Cmd::Concluded { .. } => {}
                }
            }
            self.inner
                .deliver(
                    &self.session,
                    Batch {
                        carrier: self.carrier.clone(),
                        facts,
                        live,
                        checkpoint: None,
                    },
                )
                .await;
            return true;
        }
        if facts.is_empty() && live.is_empty() {
            return false;
        }
        let checkpoint = (!facts.is_empty()).then(|| self.checkpoint(through));
        self.inner
            .deliver(
                &self.session,
                Batch {
                    carrier: self.carrier.clone(),
                    facts,
                    live,
                    checkpoint,
                },
            )
            .await;
        false
    }
}

impl BackendAdapter for ClaudeBackend {
    fn models(&self, cwd: std::path::PathBuf) -> nd_backend::ModelQuery<'_> {
        let inner = self.inner.clone();
        Box::pin(async move {
            // 请求连接消失也继续完成有界的查询和清理，不能留下辅助进程。
            tokio::spawn(async move {
                let _guard = inner.model_query.lock().await;
                crate::models::query(&inner.claude, &inner.claims, inner.generation, cwd)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())?
        })
    }
    fn kind(&self) -> BackendKind {
        BackendKind::Claude
    }

    fn adopt(&self, part: AdoptPart) {
        self.inner
            .inboxes
            .lock()
            .unwrap()
            .insert(part.session.clone(), part.inbox.clone());
        if part.carriers.is_empty() && part.pending.is_empty() {
            return;
        }
        // 接回是异步的；执行器这时交来的票先排进承载位的队列，不回 Gone。
        let queues = part
            .carriers
            .iter()
            .filter(|c| c.run.is_some())
            .map(|c| {
                (
                    c.carrier.clone(),
                    self.inner.reserve(&part.session, &c.carrier),
                )
            })
            .collect();
        let inner = self.inner.clone();
        self.inner.runtime.spawn(inner.adopt(part, queues));
    }

    fn act(&self, issued: Issued, act: Act) -> Admit {
        let accepted = Admit::Accepted {
            may_be_unknown: true,
        };
        match act {
            Act::Open { carrier, run, spec } => {
                if spec.profile.kind != BackendKind::Claude {
                    return Admit::Rejected {
                        reject: Reject::Unsupported {
                            why: "Claude 适配只拉起 Claude 后端".into(),
                        },
                    };
                }
                let inner = self.inner.clone();
                self.inner
                    .runtime
                    .spawn(inner.open(issued, carrier, run, spec));
                accepted
            }
            Act::Invoke { to, invocation } => match self.inner.carriers.lock().unwrap().get(&to) {
                Some(slot)
                    if slot.session == issued.session
                        && slot
                            .tx
                            .send(if titles(&invocation) {
                                Cmd::Title {
                                    ticket: issued.ticket.clone(),
                                    invocation,
                                    write: true,
                                }
                            } else {
                                Cmd::Invoke {
                                    ticket: issued.ticket.clone(),
                                    invocation,
                                }
                            })
                            .is_ok() =>
                {
                    accepted
                }
                _ => Admit::Rejected {
                    reject: Reject::Gone,
                },
            },
            Act::Configure { to, setting } => {
                let carriers = self.inner.carriers.lock().unwrap();
                match carriers.get(&to) {
                    Some(slot)
                        if slot.session == issued.session
                            && slot
                                .tx
                                .send(Cmd::Configure {
                                    ticket: issued.ticket.clone(),
                                    setting,
                                })
                                .is_ok() =>
                    {
                        accepted
                    }
                    _ => Admit::Rejected {
                        reject: Reject::Gone,
                    },
                }
            }
            Act::Interrupt { ref to, .. } | Act::Withdraw { ref to, .. } => {
                let carriers = self.inner.carriers.lock().unwrap();
                match carriers.get(to) {
                    Some(slot)
                        if slot.session == issued.session
                            && slot
                                .tx
                                .send(Cmd::Control {
                                    ticket: issued.ticket,
                                    act,
                                })
                                .is_ok() =>
                    {
                        accepted
                    }
                    _ => Admit::Rejected {
                        reject: Reject::Gone,
                    },
                }
            }
            Act::Send { to, msg } => {
                let carriers = self.inner.carriers.lock().unwrap();
                match carriers.get(&to) {
                    Some(slot) if slot.session == issued.session => {
                        // 普通输入最多 128 条；控制和流水确认不占这份容量。
                        if slot
                            .normal_queued
                            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                (n < 128).then_some(n + 1)
                            })
                            .is_err()
                        {
                            return Admit::Rejected {
                                reject: Reject::Busy,
                            };
                        }
                        let permit = NormalPermit(slot.normal_queued.clone());
                        if slot
                            .tx
                            .send(Cmd::Send {
                                ticket: issued.ticket,
                                msg,
                                _permit: permit,
                            })
                            .is_ok()
                        {
                            accepted
                        } else {
                            Admit::Rejected {
                                reject: Reject::Gone,
                            }
                        }
                    }
                    _ => Admit::Rejected {
                        reject: Reject::Gone,
                    },
                }
            }
            Act::End { carrier, how } => {
                let sent = self
                    .inner
                    .carriers
                    .lock()
                    .unwrap()
                    .get(&carrier)
                    .map(|slot| {
                        slot.tx
                            .send(Cmd::End {
                                ticket: issued.ticket.clone(),
                                how,
                            })
                            .is_ok()
                    });
                if sent != Some(true) {
                    // 没有活进程：结束本来就成立。
                    let inner = self.inner.clone();
                    self.inner.runtime.spawn(async move {
                        inner
                            .deliver_facts(
                                &issued.session,
                                &carrier,
                                vec![done(
                                    &issued.ticket,
                                    Outcome::Ok {
                                        done: Done::Ended { code: None },
                                    },
                                )],
                            )
                            .await;
                    });
                }
                accepted
            }
        }
    }

    fn committed(&self, ack: Ack) {
        if let Some(seq) = ack.checkpoint.0["seq"].as_u64()
            && let Some(slot) = self.inner.carriers.lock().unwrap().get(&ack.carrier)
        {
            let _ = slot.tx.send(Cmd::Ack(seq));
        }
    }

    fn release(&self, session: &SessionId) {
        self.inner.inboxes.lock().unwrap().remove(session);
    }
}

fn settings_with_caps(mut settings: Value) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    // 缺字段即不可用；ACK 不能代替实际状态。Codex 适配没有这个能力。
    let ultra = settings["applied"]["ultracodeAvailable"] == true
        && settings["applied"]["ultracodeRequested"].is_boolean()
        && settings["applied"]["ultracode"].is_boolean();
    settings = json!({"applied":settings["applied"],"permission_modes":["default","acceptEdits","plan","dontAsk","auto","bypassPermissions"]});
    settings["caps"] = json!({"model":settings["applied"]["model"].is_string(),"effort":settings["applied"].is_object(),"permission_mode":settings["applied"].is_object(),"ultracode":ultra});
    settings
}

fn setting_request(setting: &nd_wire::LiveSetting) -> Value {
    match setting {
        nd_wire::LiveSetting::Model(model) => json!({"subtype":"set_model","model":model}),
        nd_wire::LiveSetting::Effort(effort) => {
            json!({"subtype":"apply_flag_settings","settings":{"effortLevel":effort}})
        }
        nd_wire::LiveSetting::Ultracode(on) => {
            json!({"subtype":"apply_flag_settings","settings":{"ultracode":on}})
        }
        nd_wire::LiveSetting::PermissionMode(mode) => {
            json!({"subtype":"set_permission_mode","mode":mode})
        }
    }
}

// 仅用于 Open 阶段：还没有提交给此进程的人类提示，因此不会吞对话流水。
async fn initial_control(run: &mut ClaudeRun, request: Value) -> Result<Value, String> {
    let id = format!("initial-settings-{}", run.next_input());
    run.write(&json!({"type":"control_request","request_id":id,"request":request}))
        .await
        .map_err(|e| e.to_string())?;
    let frames = run
        .wait_frame(Duration::from_secs(30), |f| {
            f["type"] == "control_response" && f["response"]["request_id"] == id
        })
        .await
        .map_err(|e| e.to_string())?;
    let response = &frames.last().ok_or("设置没有回应")?["response"];
    if response["subtype"] != "success" {
        return Err(response.to_string());
    }
    Ok(response["response"].clone())
}
