//! Claude 适配：后端端口 [`BackendAdapter`] 的 Claude 实现。
//!
//! 每个活的承载位一个任务，持有它的看守连接（唯一的 stdin 写入者）：交来的票按到达次序写出，
//! 定时读看守流水，经 [`Conversation`] 归一成事实，按批交到会话的收件地址；
//! 会话提交了带检查点的批次之后才给看守确认。拉起、退出都向独占登记报观察（`Up`、`Gone`）。
//!
//! 我方发起的原生编号由票派生（user 行的 uuid 是 `native_uuid(票)`），送达只认带原 uuid 的回显。
use crate::{
    Caps, Claude, ClaudeRun, InitOptions, Open, Readiness as ClaudeReadiness, Start,
    convo::{Conversation, Convo},
};
use nd_backend::{
    Ack, Act, Admit, AdoptPart, BackendAdapter, BackendKind, Batch, CarrierId, CarrierRecord,
    Checkpoint, Done, Drain, EndHow, Fact, FactBody, Inbox, Intent, Issued, Live, Msg, OpenSpec,
    Origin, Outcome, PendingTicket, Readiness, Refusal, Reject, RunId, SessionId, Ticket,
    native_uuid,
};
use nd_claims::{Exclusivity, GoneHow, Observed};
use nd_runs::{Found, Watchdogs};
use nd_watchdog_proto::{Event, Finish};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
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

enum Cmd {
    Send { ticket: Ticket, msg: Msg },
    End { ticket: Ticket, how: EndHow },
    Ack(u64),
}

struct Slot {
    session: SessionId,
    tx: mpsc::UnboundedSender<Cmd>,
}

struct Inner {
    claude: Claude,
    watchdogs: Arc<Watchdogs>,
    claims: Arc<Exclusivity>,
    generation: u64,
    runtime: tokio::runtime::Handle,
    config: ClaudeBackendConfig,
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
                        && let Some(uuid) = frame["uuid"].as_str()
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
        session: SessionId,
        carrier: CarrierId,
        run: ClaudeRun,
        convo: Conversation,
        pending: HashMap<String, Ticket>,
        writes: HashMap<String, u64>,
        recover_through: Option<u64>,
        rx: mpsc::UnboundedReceiver<Cmd>,
    ) {
        let actor = Actor {
            inner: self.clone(),
            session,
            carrier,
            run_id: RunId(run.run().to_owned()),
            bs: run.ready().backend_session_id.clone(),
            run,
            convo,
            pending,
            writes,
            ending: None,
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
            Ok(claude_run) => {
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
                let adopt = serde_json::to_value(&ready.caps).unwrap_or(Value::Null);
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
                                    adopt,
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
                Act::Send { .. } => Outcome::Refused {
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
        let sends: HashMap<String, Ticket> = pending
            .iter()
            .filter(|p| matches!(p.act, Act::Send { .. }))
            .map(|p| (native_uuid(&p.issued.ticket), p.issued.ticket.clone()))
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
            .written(&run, &sends.keys().cloned().collect(), cursor)
            .await;
        written.extend(scanned);
        let found = self.inspect(&run).await;
        let caps: Option<Caps> = serde_json::from_value(record.adopt.clone()).ok();
        let bs = record.bs.as_ref().map(|b| b.id.clone()).unwrap_or_default();
        let adopted = match (&found, caps) {
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
            for p in &pending {
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
                    Cmd::Send { ticket, .. } => facts.push(done(
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
                    Cmd::Ack(_) => {}
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
        if !facts.is_empty() {
            self.deliver_facts(session, &record.carrier, facts).await;
        }
        self.spawn_actor(
            session.clone(),
            record.carrier.clone(),
            claude_run,
            convo,
            waiting,
            written,
            Some(recover_through),
            queued,
        );
        for p in &pending {
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
    writes: HashMap<String, u64>,
    ending: Option<Ticket>,
    recover_through: Option<u64>,
    rx: mpsc::UnboundedReceiver<Cmd>,
}

impl Actor {
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
        loop {
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    Some(cmd) => self.command(cmd).await,
                    None => return,
                },
                _ = tokio::time::sleep(self.inner.config.poll) => {
                    if self.poll().await {
                        return;
                    }
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
            Cmd::Send { ticket, msg } => {
                let uuid = native_uuid(&ticket);
                let priority = match msg.intent {
                    Intent::Fold => "next",
                    Intent::AfterTurn => "later",
                    Intent::Interrupting => "now",
                };
                let frame = json!({
                    "type": "user",
                    "uuid": uuid,
                    "session_id": self.bs,
                    "parent_tool_use_id": null,
                    "message": {"role": "user", "content": [{"type": "text", "text": msg.text}]},
                    "priority": priority,
                    "origin": {"kind": "human"},
                });
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
            Cmd::Ack(seq) => {
                if self.run.ack(seq).await.is_err() {
                    self.relink().await;
                }
            }
        }
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
            if let Event::In { in_seq, line } = &record.event
                && let Ok(frame) = serde_json::from_str::<Value>(line)
                && let Some(uuid) = frame["uuid"].as_str()
                && self.pending.contains_key(uuid)
            {
                self.writes.insert(uuid.to_owned(), *in_seq);
            }
            for (n, convo) in self.convo.apply(record).into_iter().enumerate() {
                let key = format!("{}:{}:{n}", self.run_id, record.seq);
                match convo {
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
                    Convo::Lifecycle { .. } | Convo::Reply { .. } => {}
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
                    } => facts.push(fact(
                        key,
                        FactBody::Asked {
                            id: request_id,
                            kind: subtype,
                            raw,
                        },
                    )),
                    Convo::Tasks { drain } => facts.push(fact(key, FactBody::Tasks { drain })),
                    Convo::Gap { lost } => facts.push(fact(key, FactBody::Gap { lost })),
                    Convo::Exit { code } => exited = Some(code),
                }
            }
        }
        if let Some(code) = exited {
            for (_, ticket) in self.pending.drain() {
                facts.push(done(
                    &ticket,
                    Outcome::Unknown {
                        evidence: "写出后进程退出，没有等到回显".into(),
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
            while let Ok(cmd) = self.rx.try_recv() {
                match cmd {
                    Cmd::Send { ticket, .. } => facts.push(done(
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
                    Cmd::Ack(_) => {}
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
        let checkpoint = (!facts.is_empty()).then(|| {
            Checkpoint(json!({"seq": through, "convo": self.convo, "writes": self.writes}))
        });
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
            Act::Send { to, msg } => {
                let carriers = self.inner.carriers.lock().unwrap();
                match carriers.get(&to) {
                    Some(slot)
                        if slot.session == issued.session
                            && slot
                                .tx
                                .send(Cmd::Send {
                                    ticket: issued.ticket.clone(),
                                    msg,
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
