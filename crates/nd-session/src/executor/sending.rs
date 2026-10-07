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
    pub(super) fn pump(&mut self, tx: &mut Tx<'_>, fx: &mut Effects) -> nd_store::Result<bool> {
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
        let structural = self.core.ops.values().any(|op| op.spec.structural());
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
                        issuer: Issuer::Message {
                            id: m.id.clone(),
                            arrival: m.arrival,
                        },
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
