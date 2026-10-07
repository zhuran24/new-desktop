use super::*;

impl Executor {
    pub(super) fn ensure_lineage(
        &mut self,
        carrier: &nd_backend::CarrierId,
    ) -> nd_store::Result<()> {
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

    pub(super) fn batch(
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
                FactBody::CanCancelQueued { available } => {
                    if let Some(c) = self.core.carriers.get_mut(&batch.carrier) {
                        c.interaction.cancel_queued = available;
                    }
                }
                FactBody::TitleChanged { title } => {
                    if self.core.current.as_ref() == Some(&batch.carrier) {
                        let meta = self.core.meta.as_mut().unwrap();
                        meta.title = Some(title);
                        meta.title_source = Some("manual".into());
                    }
                }
                FactBody::Recovered => {
                    self.recovering.remove(&batch.carrier);
                }
                FactBody::Clarified { ticket, outcome } => {
                    let confirmed = self
                        .core
                        .uncertain
                        .get(&ticket)
                        .is_some_and(|row| match &row.act {
                            Act::Send { .. } => matches!(
                                &outcome,
                                Outcome::Ok {
                                    done: Done::Landed { .. }
                                } | Outcome::Refused {
                                    refusal: Refusal::Lost { .. }
                                }
                            ),
                            Act::Withdraw { .. } => matches!(
                                &outcome,
                                Outcome::Ok {
                                    done: Done::Withdrawn { .. }
                                } | Outcome::Failed { .. }
                                    | Outcome::Rejected { .. }
                                    | Outcome::Refused {
                                        refusal: Refusal::Lost { .. }
                                    }
                            ),
                            Act::Interrupt { .. } => matches!(
                                &outcome,
                                Outcome::Ok {
                                    done: Done::Interrupted { .. }
                                } | Outcome::Failed { .. }
                                    | Outcome::Rejected { .. }
                                    | Outcome::Refused {
                                        refusal: Refusal::Lost { .. }
                                    }
                            ),
                            _ => false,
                        });
                    if confirmed && let Some(mut row) = self.core.uncertain.remove(&ticket) {
                        row.outcome = None;
                        self.core.outbox.insert(ticket.clone(), row);
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
                FactBody::SettingsObserved { settings } => {
                    self.core.meta.as_mut().unwrap().settings = settings;
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
                    let Some(backend_session) =
                        self.core.carriers.get(&batch.carrier).map(|c| c.bs.clone())
                    else {
                        continue;
                    };
                    self.core.lineage = self
                        .core
                        .lineage
                        .fold(&LineageEvent::TurnObserved {
                            carrier: batch.carrier.clone(),
                            backend_session,
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
                        self.core.lineage = self
                            .core
                            .lineage
                            .fold(&LineageEvent::CarrierExited {
                                carrier: batch.carrier.clone(),
                                backend_session: c.bs.clone(),
                            })
                            .map_err(aborted)?;
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
    pub(super) fn record_outcome(
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
                            interaction,
                            adopt,
                            features,
                            settings,
                            ..
                        },
                },
            ) => {
                if let Some(c) = self.core.carriers.get_mut(carrier) {
                    c.run = Some(run.clone());
                    c.alive = true;
                    c.readiness = Some(readiness.clone());
                    c.interaction = interaction.clone();
                    c.features = features.clone();
                    c.adopt = adopt.clone();
                    c.checkpoint = None;
                    c.turn_running = false;
                }
                let meta = self.core.meta.as_mut().unwrap();
                meta.settings = settings.clone();
                if let Some(mode) = &settings.permission_mode {
                    meta.permission_mode = Some(mode.clone());
                }
                self.ensure_lineage(carrier)?;
            }
            (
                Act::Invoke {
                    invocation: nd_backend::Invocation::GenerateTitle { .. },
                    ..
                },
                Outcome::Ok {
                    done: Done::Titled { title: Some(title) },
                },
            ) => {
                let meta = self.core.meta.as_mut().unwrap();
                if meta.title_source.as_deref() != Some("manual") {
                    meta.title = Some(title.clone());
                    meta.title_source = Some("ai".into());
                }
            }
            (
                Act::Configure { setting, .. },
                Outcome::Ok {
                    done: Done::Configured { settings },
                },
            ) => {
                let meta = self.core.meta.as_mut().unwrap();
                match setting {
                    nd_wire::LiveSetting::Model(model) => meta.model = Some(model.clone()),
                    nd_wire::LiveSetting::Effort(effort) => meta.effort = Some(effort.clone()),
                    nd_wire::LiveSetting::PermissionMode(mode) => {
                        meta.permission_mode = Some(mode.clone())
                    }
                    _ => {}
                }
                let models = meta.settings.models.clone();
                meta.settings = settings.clone();
                meta.settings.models = models;
                meta.settings.permission_mode = meta.permission_mode.clone();
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
                Outcome::Refused {
                    refusal: Refusal::Withdrawn,
                } => ("withdrawn", None, None),
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
                    refusal: Refusal::Withheld | Refusal::Withdrawn
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
            Issuer::Withdrawal {
                id,
                message,
                restore,
            } => {
                if !matches!(outcome, Outcome::Unknown { .. }) {
                    self.pin_return(tx, &id, &restore, false)?;
                }
                let state = match &outcome {
                    Outcome::Ok {
                        done: Done::Withdrawn { ok: true },
                    } => {
                        self.restore_withdrawn(tx, fx, &message)?;
                        self.refill_draft(tx, &id, &restore, std::slice::from_ref(&message))?;
                        "withdrawn"
                    }
                    Outcome::Ok {
                        done: Done::Withdrawn { ok: false },
                    } => {
                        if self.core.messages.contains_key(&message.id) {
                            self.show_message(
                                tx,
                                fx,
                                &message,
                                "written",
                                Some("已开始处理，撤回失败".into()),
                            )?;
                        }
                        "not_withdrawable"
                    }
                    Outcome::Unknown { .. } => {
                        if self.core.messages.contains_key(&message.id) {
                            self.show_message(tx, fx, &message, "unknown", Some(outcome.reason()))?;
                        }
                        "unknown"
                    }
                    _ => {
                        if self.core.messages.contains_key(&message.id) {
                            self.show_message(tx, fx, &message, "written", Some(outcome.reason()))?;
                        }
                        "failed"
                    }
                };
                self.show(
                    tx,
                    fx,
                    Shown::Control {
                        id,
                        state: state.into(),
                        outcome: serde_json::to_value(outcome).map_err(aborted)?,
                    },
                )?;
            }
            Issuer::Control {
                id,
                restore,
                mut held,
            } => {
                if let Outcome::Ok {
                    done: Done::Interrupted { cancelled },
                } = &outcome
                {
                    for send in cancelled {
                        if let Some(message) = self
                            .core
                            .messages
                            .values()
                            .find(|m| m.ticket.as_ref() == Some(send))
                        {
                            held.push(message.clone());
                        } else if let Some(row) = self.core.uncertain.get(send)
                            && let (Act::Send { msg, .. }, Issuer::Message { id, arrival }) =
                                (&row.act, &row.issuer)
                        {
                            held.push(Message {
                                id: id.clone(),
                                text: msg.text.clone(),
                                attachments: msg.attachments.clone(),
                                intent: msg.intent,
                                ticket: Some(send.clone()),
                                attempt: 0,
                                waiting: None,
                                arrival: *arrival,
                            });
                        }
                    }
                }
                held.sort_by_key(|m| m.arrival);
                for message in &held {
                    self.restore_withdrawn(tx, fx, message)?;
                }
                if let Some(restore) = restore {
                    self.refill_draft(tx, &id, &restore, &held)?;
                    if !matches!(outcome, Outcome::Unknown { .. }) {
                        self.pin_return(tx, &id, &restore, false)?;
                    }
                }
                if let Some(row) = self.core.uncertain.get_mut(ticket)
                    && let Issuer::Control { held, .. } = &mut row.issuer
                {
                    held.clear();
                }
                let state = match &outcome {
                    Outcome::Ok { .. } => "acknowledged",
                    Outcome::Unknown { .. } => "unknown",
                    _ => "failed",
                };
                self.show(
                    tx,
                    fx,
                    Shown::Control {
                        id,
                        state: state.into(),
                        outcome: serde_json::to_value(outcome).map_err(aborted)?,
                    },
                )?;
            }
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
            Issuer::Message { id, .. } => {
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
}
