//! 脚本化适配器：窄接缝一（引擎崩溃矩阵）里唯一的替身，实现 [`BackendAdapter`]。
//!
//! 它代表「后端」：执行器崩溃、重开时它不丢状态，按脚本决定每张票何时、以什么结果回来，
//! 并分三层记录：接纳（`received`）、原生步骤的写出（`applied`）、传输层的重发（`redelivered`）。
//! 恢复时 `adopt` 交来的未结票：收到过的重报结果；没收到过的按动作的 `if_unsent` 处理——
//! 写类动作证明没写出（`Refused(Withheld)`），其余照做。
use nd_backend::{
    Act, Admit, AdoptPart, BackendAdapter, BackendKind, Batch, CarrierId, Done, Drain, Fact,
    FactBody, IfUnsent, Inbox, Issued, Outcome, Readiness, Refusal, RunId, SessionId, Ticket,
};
use nd_claims::{Exclusivity, GoneHow, Identity, Observed};
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

/// 一张票的脚本：成功、明确失败、交付不明，或扣住等测试放行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Ok,
    Fail(String),
    Unknown(String),
    Lost(String),
    Hold,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActKind {
    Open,
    End,
    Send,
    Configure,
    Title,
}
fn kind_of(act: &Act) -> ActKind {
    match act {
        Act::Open { .. } => ActKind::Open,
        Act::End { .. } => ActKind::End,
        Act::Send { .. } => ActKind::Send,
        Act::Configure { .. } => ActKind::Configure,
        Act::Invoke { .. } => ActKind::Title,
    }
}

#[derive(Default)]
struct Inner {
    inboxes: HashMap<SessionId, Inbox>,
    script: HashMap<ActKind, VecDeque<Reply>>,
    received: Vec<(Ticket, Act)>,
    applied: Vec<String>,
    redelivered: Vec<Ticket>,
    results: HashMap<Ticket, (SessionId, CarrierId, Vec<Fact>)>,
    held: Vec<(Issued, Act)>,
    live: HashMap<CarrierId, RunId>,
    drain: Option<Drain>,
    acks: usize,
}

/// 重启之间留着的后端状态。
pub struct Detached {
    inner: Arc<Mutex<Inner>>,
    identity: Identity,
}
impl Detached {
    pub fn attach(self, claims: Arc<Exclusivity>) -> Arc<ScriptedAdapter> {
        Arc::new(ScriptedAdapter {
            inner: self.inner,
            claims,
            identity: self.identity,
        })
    }
}

pub struct ScriptedAdapter {
    inner: Arc<Mutex<Inner>>,
    claims: Arc<Exclusivity>,
    identity: Identity,
}

impl ScriptedAdapter {
    /// 进程身份用测试进程自己的（真的、活着的）；独占登记据它记自有进程。
    pub fn new(claims: Arc<Exclusivity>) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Mutex::new(Inner {
                drain: Some(Drain::Drained),
                ..Inner::default()
            })),
            claims,
            identity: Identity::read(std::process::id()).expect("own identity"),
        })
    }
    /// 守护进程「重启」：后端（这个适配器的状态）留着，旧的独占登记实例放手。
    pub fn reattach_detached(&self) -> Detached {
        Detached {
            inner: self.inner.clone(),
            identity: self.identity.clone(),
        }
    }
    pub fn identity(&self) -> Identity {
        self.identity.clone()
    }
    /// 下一张这种动作的票按这个脚本回；没排的按 `Ok`。
    pub fn script(&self, kind: ActKind, reply: Reply) {
        self.inner
            .lock()
            .unwrap()
            .script
            .entry(kind)
            .or_default()
            .push_back(reply);
    }
    /// 拉起之后报的后台任务状态。
    pub fn set_drain(&self, drain: Drain) {
        self.inner.lock().unwrap().drain = Some(drain);
    }
    pub fn received(&self) -> Vec<(Ticket, Act)> {
        self.inner.lock().unwrap().received.clone()
    }
    /// 原生步骤（真的生效了的）：`open:<run>`、`send:<文字>`、`end:<run>`。
    pub fn applied(&self) -> Vec<String> {
        self.inner.lock().unwrap().applied.clone()
    }
    pub fn redelivered(&self) -> Vec<Ticket> {
        self.inner.lock().unwrap().redelivered.clone()
    }
    pub fn live(&self) -> HashMap<CarrierId, RunId> {
        self.inner.lock().unwrap().live.clone()
    }
    /// 后端取得新证据，更新已报 Unknown 的票；可重复报告，执行器负责幂等。
    pub fn clarify(&self, ticket: &Ticket, outcome: Outcome) {
        let updated = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .results
                .get_mut(ticket)
                .map(|(session, carrier, facts)| {
                    let f = fact(
                        &format!("clarified:{ticket}"),
                        FactBody::Clarified {
                            ticket: ticket.clone(),
                            outcome,
                        },
                    );
                    facts.push(f.clone());
                    (session.clone(), carrier.clone(), f)
                })
        };
        if let Some((session, carrier, f)) = updated {
            self.deliver(&session, &carrier, vec![f]);
        }
    }
    pub fn held(&self) -> Vec<(Ticket, Act)> {
        self.inner
            .lock()
            .unwrap()
            .held
            .iter()
            .map(|(i, a)| (i.ticket.clone(), a.clone()))
            .collect()
    }
    /// 放出扣住的第一张票，按给的结果回。
    pub fn release(&self, reply: Reply) -> bool {
        let next = {
            let mut inner = self.inner.lock().unwrap();
            if inner.held.is_empty() {
                None
            } else {
                Some(inner.held.remove(0))
            }
        };
        match next {
            Some((issued, act)) => {
                self.perform(issued, act, reply);
                true
            }
            None => false,
        }
    }
    /// 模拟后端进程意外退出：独占登记记它离开，报 `Exited`。
    pub fn crash_process(&self, carrier: &CarrierId) {
        let (run, session) = {
            let mut inner = self.inner.lock().unwrap();
            let Some(run) = inner.live.remove(carrier) else {
                return;
            };
            let session = inner
                .results
                .values()
                .find(|(_, c, _)| c == carrier)
                .map(|(s, _, _)| s.clone());
            (run, session)
        };
        self.gone(&run);
        if let Some(session) = session {
            self.deliver(
                &session,
                carrier,
                vec![fact(
                    &format!("exit:{run}"),
                    FactBody::Exited {
                        run: run.clone(),
                        code: Some(137),
                    },
                )],
            );
        }
    }

    fn gone(&self, run: &RunId) {
        let _ = self.claims.observe(Observed::Gone {
            run: run.0.clone(),
            identity: Some(self.identity.clone()),
            how: GoneHow::Exited,
        });
    }

    fn deliver(&self, session: &SessionId, carrier: &CarrierId, facts: Vec<Fact>) {
        let inbox = self.inner.lock().unwrap().inboxes.get(session).cloned();
        if let Some(inbox) = inbox {
            let _ = inbox.try_send(Batch {
                carrier: carrier.clone(),
                facts,
                live: vec![],
                checkpoint: None,
            });
        }
    }

    fn perform(&self, issued: Issued, act: Act, reply: Reply) {
        let carrier = act.carrier().clone();
        let ticket = issued.ticket.clone();
        let done = |outcome: Outcome| {
            fact(
                &format!("done:{ticket}"),
                FactBody::Done {
                    ticket: ticket.clone(),
                    outcome,
                },
            )
        };
        let facts = match (&act, reply) {
            (_, Reply::Hold) => {
                self.inner.lock().unwrap().held.push((issued, act));
                return;
            }
            (Act::Open { run, spec, .. }, Reply::Ok) => {
                let _ = self.claims.observe(Observed::Up {
                    run: run.0.clone(),
                    identity: self.identity.clone(),
                    generation: 1,
                    kind: BackendKind::Claude,
                });
                let mut inner = self.inner.lock().unwrap();
                inner.applied.push(format!("open:{run}"));
                inner.live.insert(carrier.clone(), run.clone());
                let drain = inner.drain.clone().unwrap_or(Drain::Drained);
                drop(inner);
                vec![
                    done(Outcome::Ok {
                        done: Done::Opened {
                            bs: spec.origin.backend_session().clone(),
                            run: run.clone(),
                            readiness: Readiness::Full,
                            adopt: json!({"scripted": true}),
                        },
                    }),
                    fact(&format!("tasks:{ticket}"), FactBody::Tasks { drain }),
                ]
            }
            (Act::Open { run, .. }, Reply::Fail(why)) => {
                let _ = self.claims.observe(Observed::Gone {
                    run: run.0.clone(),
                    identity: None,
                    how: GoneHow::NeverLaunched,
                });
                vec![done(Outcome::failed(why))]
            }
            (Act::Send { msg, .. }, Reply::Ok) => {
                let native = nd_backend::native_uuid(&ticket);
                self.inner
                    .lock()
                    .unwrap()
                    .applied
                    .push(format!("send:{}", msg.text));
                vec![
                    fact(
                        &format!("written:{ticket}"),
                        FactBody::Written {
                            ticket: ticket.clone(),
                            native: native.clone(),
                        },
                    ),
                    done(Outcome::Ok {
                        done: Done::Landed { native },
                    }),
                ]
            }
            (Act::Send { msg, .. }, Reply::Unknown(why)) => {
                // 写出了，回显没来，进程就没了：可能已生效。
                self.inner
                    .lock()
                    .unwrap()
                    .applied
                    .push(format!("send?:{}", msg.text));
                vec![done(Outcome::Unknown { evidence: why })]
            }
            (Act::Configure { setting, .. }, Reply::Ok) => {
                self.inner
                    .lock()
                    .unwrap()
                    .applied
                    .push(format!("configure:{setting:?}"));
                let applied = serde_json::to_value(setting).unwrap();
                vec![done(Outcome::Ok {
                    done: Done::Configured {
                        settings: json!({"applied":applied}),
                    },
                })]
            }
            (
                Act::Invoke {
                    invocation: nd_backend::Invocation::Title { title },
                    ..
                },
                Reply::Ok,
            ) => {
                self.inner
                    .lock()
                    .unwrap()
                    .applied
                    .push(format!("title:{title}"));
                vec![
                    fact(
                        &format!("title:{ticket}"),
                        FactBody::TitleChanged {
                            title: title.clone(),
                        },
                    ),
                    done(Outcome::Ok {
                        done: Done::Titled {
                            title: Some(title.clone()),
                        },
                    }),
                ]
            }
            (
                Act::Invoke {
                    invocation: nd_backend::Invocation::GenerateTitle { .. },
                    ..
                },
                Reply::Ok,
            ) => {
                self.inner
                    .lock()
                    .unwrap()
                    .applied
                    .push("generate-title".into());
                vec![done(Outcome::Ok {
                    done: Done::Titled {
                        title: Some("生成的标题".into()),
                    },
                })]
            }
            (Act::End { .. }, Reply::Ok) => {
                let run = self.inner.lock().unwrap().live.remove(&carrier);
                let mut facts = vec![done(Outcome::Ok {
                    done: Done::Ended { code: Some(0) },
                })];
                if let Some(run) = run {
                    self.gone(&run);
                    self.inner
                        .lock()
                        .unwrap()
                        .applied
                        .push(format!("end:{run}"));
                    facts.push(fact(
                        &format!("exit:{run}"),
                        FactBody::Exited { run, code: Some(0) },
                    ));
                }
                facts
            }
            (_, Reply::Lost(evidence)) => vec![done(Outcome::Refused {
                refusal: Refusal::Lost { evidence },
            })],
            (_, Reply::Fail(why)) => vec![done(Outcome::failed(why))],
            (_, Reply::Unknown(why)) => vec![done(Outcome::Unknown { evidence: why })],
        };
        self.inner.lock().unwrap().results.insert(
            ticket,
            (issued.session.clone(), carrier.clone(), facts.clone()),
        );
        self.deliver(&issued.session, &carrier, facts);
    }
}

fn fact(key: &str, body: FactBody) -> Fact {
    Fact {
        key: key.into(),
        body,
    }
}

impl BackendAdapter for ScriptedAdapter {
    fn kind(&self) -> BackendKind {
        BackendKind::Claude
    }
    fn adopt(&self, part: AdoptPart) {
        self.inner
            .lock()
            .unwrap()
            .inboxes
            .insert(part.session.clone(), part.inbox.clone());
        for record in &part.carriers {
            let alive = self.inner.lock().unwrap().live.get(&record.carrier) == record.run.as_ref();
            if !alive && let Some(run) = &record.run {
                self.deliver(
                    &part.session,
                    &record.carrier,
                    vec![fact(
                        &format!("exit:{run}"),
                        FactBody::Exited {
                            run: run.clone(),
                            code: None,
                        },
                    )],
                );
            }
        }
        let recovered: std::collections::HashSet<_> = part
            .carriers
            .iter()
            .map(|c| c.carrier.clone())
            .chain(part.pending.iter().map(|p| p.act.carrier().clone()))
            .collect();
        for pending in part.pending {
            let ticket = pending.issued.ticket.clone();
            let (seen, result, held) = {
                let inner = self.inner.lock().unwrap();
                (
                    inner.received.iter().any(|(t, _)| t == &ticket),
                    inner.results.get(&ticket).cloned(),
                    inner.held.iter().any(|(i, _)| i.ticket == ticket),
                )
            };
            match (seen, result) {
                (true, Some((session, carrier, facts))) => {
                    self.inner.lock().unwrap().redelivered.push(ticket);
                    self.deliver(&session, &carrier, facts);
                }
                (true, None) if held => {}
                (true, None) => {}
                (false, _) => match pending.act.if_unsent() {
                    IfUnsent::Withhold => self.deliver(
                        &part.session,
                        pending.act.carrier(),
                        vec![fact(
                            &format!("withheld:{ticket}"),
                            FactBody::Done {
                                ticket: ticket.clone(),
                                outcome: Outcome::Refused {
                                    refusal: Refusal::Withheld,
                                },
                            },
                        )],
                    ),
                    IfUnsent::Resend => {
                        let _ = self.act(pending.issued, pending.act);
                    }
                },
            }
        }
        for carrier in recovered {
            self.deliver(
                &part.session,
                &carrier,
                vec![fact("recovered", FactBody::Recovered)],
            );
        }
    }
    fn act(&self, issued: Issued, act: Act) -> Admit {
        let reply = {
            let mut inner = self.inner.lock().unwrap();
            if inner.received.iter().any(|(t, _)| t == &issued.ticket) {
                return Admit::Accepted {
                    may_be_unknown: true,
                };
            }
            inner.received.push((issued.ticket.clone(), act.clone()));
            inner
                .script
                .get_mut(&kind_of(&act))
                .and_then(VecDeque::pop_front)
                .unwrap_or(Reply::Ok)
        };
        self.perform(issued, act, reply);
        Admit::Accepted {
            may_be_unknown: true,
        }
    }
    fn committed(&self, _ack: nd_backend::Ack) {
        self.inner.lock().unwrap().acks += 1;
    }
    fn release(&self, session: &SessionId) {
        self.inner.lock().unwrap().inboxes.remove(session);
    }
}
