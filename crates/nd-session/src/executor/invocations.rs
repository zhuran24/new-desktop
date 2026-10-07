use super::*;

impl Executor {
    // —— 总结、`!` 命令、fork 型子代理（收据等动作有结果）——

    /// 受理一条收据等动作有结果的命令：校验、记下意图（核心里的 `Invoke`）、按需清掉匹配的草稿。
    /// 返回 `Some` 是立即可定的拒绝收据；`None` 表示已受理，收据等结果。
    pub(super) fn invoke_command(
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
            && self.consume_draft_if_matches(tx, command, input, &[])?
        {
            invoke.draft_base = self.core.draft.version;
        }
        let shown = invoke_shown(&invoke, "held", json!({}));
        self.core.invokes.insert(invoke.id.clone(), invoke);
        self.show(tx, fx, shown)?;
        Ok(None)
    }

    /// 当前段里还是 CLI 对话行的人类提示，按出现先后。
    pub(super) fn visible_prompts(&self) -> Vec<String> {
        let lineage = &self.core.lineage;
        lineage
            .current()
            .and_then(|segment| lineage.rounds(segment).ok())
            .unwrap_or_default()
            .iter()
            .flat_map(|turn| turn.messages.clone())
            .filter(|m| !self.core.summarized.contains(m))
            .collect()
    }

    pub(super) fn prompt_content(
        &self,
        message: &str,
    ) -> Option<(String, Vec<nd_wire::Attachment>)> {
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
    pub(super) fn anchor(
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
                candidates: visible
                    .iter()
                    .filter_map(|id| self.prompt_content(id))
                    .map(|(text, attachments)| nd_backend::Msg {
                        text,
                        attachments,
                        intent: Intent::Fold,
                    })
                    .collect(),
                selected: at,
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
    pub(super) fn pump_invokes(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
    ) -> nd_store::Result<bool> {
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
                            invocation: invoke.invocation.clone(),
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

    pub(super) fn show_invoke(
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
    pub(super) fn finish_invoke(
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
    pub(super) fn backfill(
        &mut self,
        tx: &mut Tx<'_>,
        invoke: &Invoke,
        text: String,
        attachments: Vec<nd_wire::Attachment>,
    ) -> nd_store::Result<Vec<String>> {
        let mut saved = vec![];
        if invoke.draft_base != self.core.draft.version {
            if text.is_empty() && attachments.is_empty() {
                // 「总结到这里」留空：输入框已经被改过，就不动它。
                return Ok(saved);
            }
            let id = format!("{}/backfill", invoke.id);
            self.replace_or_save_draft(
                tx,
                &id,
                &invoke.device,
                invoke.draft_base,
                text,
                attachments,
            )?;
            saved.push(id);
            return Ok(saved);
        }
        let old = self.core.draft.clone();
        if (!old.text.is_empty() || !old.attachments.is_empty())
            && (old.text != text || old.attachments != attachments)
        {
            let id = format!("{}/displaced", invoke.id);
            self.reference_attachments(
                tx,
                RefOwner::SavedDraft {
                    session: &self.id,
                    command: &id,
                },
                &old.attachments,
                true,
            )?;
            self.core.draft.saved.push(nd_wire::SavedDraft {
                attachments: old.attachments.clone(),
                id: id.clone(),
                base_version: old.version,
                text: old.text.clone(),
                device: old.device.clone(),
            });
            saved.push(id);
        }
        self.replace_or_save_draft(
            tx,
            &format!("{}/backfill", invoke.id),
            &invoke.device,
            invoke.draft_base,
            text,
            attachments,
        )?;
        Ok(saved)
    }
}
