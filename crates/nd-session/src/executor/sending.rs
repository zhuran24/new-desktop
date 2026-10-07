use super::*;

impl Executor {
    pub(super) fn fail_messages(
        &mut self,
        tx: &Tx<'_>,
        fx: &mut Effects,
        reason: &str,
    ) -> nd_store::Result<()> {
        let queued: Vec<Message> = self
            .core
            .messages
            .values()
            .filter(|m| m.queue.ticket.is_none())
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

    /// 两种输入共用代持、拉起、独占放行和签票循环，并按同一个 arrival 次序交出。
    pub(super) fn pump(&mut self, tx: &mut Tx<'_>, fx: &mut Effects) -> nd_store::Result<bool> {
        let mut queued: Vec<Queued> = self
            .core
            .messages
            .values()
            .cloned()
            .map(Queued::Message)
            .chain(self.core.invokes.values().cloned().map(Queued::Invoke))
            .filter(|q| q.queue().ticket.is_none())
            .collect();
        queued.sort_by_key(|q| q.queue().arrival);
        let mut progress = false;
        for entry in queued {
            let structural = self.core.ops.values().any(|op| op.spec.structural());
            let Some(carrier) = self.core.current_carrier().cloned().filter(|_| !structural) else {
                self.show_queued(tx, fx, &entry, "held", None)?;
                continue;
            };
            if !carrier.alive {
                self.show_queued(tx, fx, &entry, "held", None)?;
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
            if let Queued::Invoke(invoke) = &entry {
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
                    self.show_queued(tx, fx, &entry, "waiting_turn", None)?;
                    continue;
                }
            }
            let ticket = entry.ticket();
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
                    let (act, issuer, display) = entry.dispatch(&carrier.id);
                    self.core.outbox.insert(
                        ticket.clone(),
                        OutRow {
                            issued: Issued {
                                ticket: ticket.clone(),
                                session: self.id.clone(),
                                write_gen: self.write_gen,
                            },
                            act,
                            issuer,
                            display,
                            kind: carrier.kind.clone(),
                            outcome: None,
                            handed: false,
                        },
                    );
                    if let Some(queue) = self.queue_mut(&entry.issuer()) {
                        queue.sent(ticket.clone());
                    }
                    fx.hand.push(ticket);
                    self.show_queued(tx, fx, &entry, "pending", None)?;
                    progress = true;
                }
                nd_claims::Admit::Go(_) => self.show_queued(tx, fx, &entry, "held", None)?,
                nd_claims::Admit::Wait(obstacle) => {
                    let why = format!("{obstacle:?}");
                    if let Some(queue) = self.queue_mut(&entry.issuer()) {
                        queue.waiting = Some(why.clone());
                    }
                    self.show_queued(tx, fx, &entry, "waiting", Some(why))?;
                }
                nd_claims::Admit::No(refusal) => {
                    let why = format!("独占登记不放行：{refusal:?}");
                    match &entry {
                        Queued::Message(m) => {
                            self.core.messages.remove(&m.id);
                            self.show_message(tx, fx, m, "failed", Some(why))?;
                        }
                        Queued::Invoke(i) => self.finish_invoke(
                            tx,
                            fx,
                            &i.id,
                            Outcome::Refused {
                                refusal: Refusal::Other { why },
                            },
                        )?,
                    }
                    progress = true;
                }
            }
        }
        Ok(progress)
    }
    pub(super) fn queue_mut(&mut self, issuer: &Issuer) -> Option<&mut state::QueueState> {
        match issuer {
            Issuer::Message { id, .. } => self.core.messages.get_mut(id).map(|m| &mut m.queue),
            Issuer::Invoke { id } => self.core.invokes.get_mut(id).map(|i| &mut i.queue),
            _ => None,
        }
    }
    fn show_queued(
        &mut self,
        tx: &Tx<'_>,
        fx: &mut Effects,
        entry: &Queued,
        state: &str,
        reason: Option<String>,
    ) -> nd_store::Result<()> {
        match entry {
            Queued::Message(m) => self.show_message(tx, fx, m, state, reason),
            Queued::Invoke(i) => self.show_invoke(tx, fx, i, state, reason),
        }
    }

    pub(super) fn show_message(
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
}

/// 调度只区分输入内容；共同状态与队列算法无须关心消息和 Invoke 的持久容器。
enum Queued {
    Message(Message),
    Invoke(Invoke),
}
impl Queued {
    fn queue(&self) -> &state::QueueState {
        match self {
            Self::Message(m) => &m.queue,
            Self::Invoke(i) => &i.queue,
        }
    }
    fn ticket(&self) -> Ticket {
        match self {
            Self::Message(m) => Ticket(format!("m:{}#{}", m.id, m.queue.attempt)),
            Self::Invoke(i) => Ticket(format!("i:{}#{}", i.id, i.queue.attempt)),
        }
    }
    fn issuer(&self) -> Issuer {
        match self {
            Self::Message(m) => Issuer::Message {
                id: m.id.clone(),
                arrival: m.queue.arrival,
            },
            Self::Invoke(i) => Issuer::Invoke { id: i.id.clone() },
        }
    }
    fn dispatch(&self, carrier: &nd_backend::CarrierId) -> (Act, Issuer, Option<String>) {
        match self {
            Self::Message(m) => (
                Act::Send {
                    to: carrier.clone(),
                    msg: nd_backend::Msg {
                        text: m.text.clone(),
                        attachments: m.attachments.clone(),
                        intent: m.intent,
                    },
                },
                self.issuer(),
                Some(m.id.clone()),
            ),
            Self::Invoke(i) => (
                Act::Invoke {
                    to: carrier.clone(),
                    invocation: i.invocation.clone(),
                },
                self.issuer(),
                None,
            ),
        }
    }
}
