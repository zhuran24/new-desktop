use super::*;

impl Executor {
    pub(super) fn command(
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
                let result = self.replace_or_save_draft(
                    tx,
                    &command.id,
                    &command.device,
                    expected.draft_version,
                    args.text,
                    attachments,
                )?;
                Ok(Receipt::Done {
                    value: json!(result),
                })
            }
            "session.interrupt" if self.born => self.interrupt(tx, command, fx),
            "session.withdraw" if self.born => self.withdraw(tx, command, fx),
            "session.rename" => {
                if let Some(expected) = command.expect.get("title_revision")
                    && expected.as_u64() != Some(self.core.meta().title.revision)
                {
                    return Ok(rejected(
                        "conflict",
                        json!({"title_revision":self.core.meta().title.revision}),
                    ));
                }
                let Some(title) = command.args["title"].as_str().filter(|s| {
                    !s.trim().is_empty()
                        && s.chars().count() <= 200
                        && !s.chars().any(char::is_control)
                }) else {
                    return Ok(rejected("invalid_title", Value::Null));
                };
                if self.core.meta().status != Status::Active
                    || self.core.ops.values().any(|op| op.spec.structural())
                {
                    return Ok(rejected("busy", Value::Null));
                }
                let carrier = self.core.current.clone().unwrap();
                self.core.meta.as_mut().unwrap().title.revision += 1;
                let op = self.start_op(
                    tx,
                    fx,
                    OpSpec::Title(crate::ops::Title {
                        carrier,
                        request: crate::ops::TitleRequest::Rename {
                            title: title.trim().into(),
                        },
                    }),
                    Some(command.id.clone()),
                )?;
                Ok(Receipt::Accepted {
                    op,
                    stream: Some(format!("session/{}", self.id)),
                })
            }
            "session.configure" => {
                if let Some(expected) = command.expect.get("settings_revision")
                    && expected.as_u64() != Some(self.core.meta().settings_revision)
                {
                    return Ok(rejected(
                        "conflict",
                        json!({"settings_revision":self.core.meta().settings_revision}),
                    ));
                }
                if self.core.meta().status != Status::Active {
                    return Ok(rejected("not_active", Value::Null));
                }
                if self.core.ops.values().any(|op| op.spec.structural()) {
                    return Ok(rejected("busy", Value::Null));
                }
                let Ok(setting) =
                    serde_json::from_value::<nd_wire::LiveSetting>(command.args["setting"].clone())
                else {
                    return Ok(rejected("invalid_setting", Value::Null));
                };
                if matches!(setting, nd_wire::LiveSetting::Ultracode(_))
                    && nd_backend::session_capabilities(
                        &self.core.meta().kind,
                        &self.core.meta().settings.caps,
                    )
                    .ultracode
                        != true
                {
                    return Ok(rejected(
                        "unsupported",
                        json!({"reason":"当前进程不支持 ultracode"}),
                    ));
                }
                let Some(carrier) = self.core.current.clone() else {
                    return Ok(rejected("not_active", Value::Null));
                };
                self.core.meta.as_mut().unwrap().settings_revision += 1;
                let op = self.start_op(
                    tx,
                    fx,
                    OpSpec::Configure(crate::ops::Configure { carrier, setting }),
                    Some(command.id.clone()),
                )?;
                Ok(Receipt::Accepted {
                    op,
                    stream: Some(format!("session/{}", self.id)),
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

    pub(super) fn attachments(
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
        if attachments.len() > nd_wire::MAX_ATTACHMENTS_PER_MESSAGE {
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
        if total > nd_wire::MAX_MESSAGE_ATTACHMENT_BYTES {
            return Err("每条消息的附件总大小不能超过 16 MiB".into());
        }
        Ok(attachments)
    }

    pub(super) fn hold_attachments(
        &self,
        tx: &mut Tx<'_>,
        command: &Command,
        attachments: &[nd_wire::Attachment],
    ) -> nd_store::Result<()> {
        self.reference_attachments(
            tx,
            RefOwner::Message {
                session: &self.id,
                command: &command.id,
            },
            attachments,
            true,
        )
    }

    pub(super) fn create(
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
                effort: None,
                permission_mode: optional("permission_mode"),
                settings: nd_backend::LiveSettings::default(),
                title: state::SessionTitle::new(text),
                settings_revision: 0,
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

    pub(super) fn return_draft(
        &self,
        tx: &Tx<'_>,
        command: &Command,
    ) -> Result<state::DraftRestore, Receipt> {
        let (version, text, attachments) = if let Some(draft) = command.args.get("draft") {
            let (Some(version), Some(text)) = (draft["version"].as_u64(), draft["text"].as_str())
            else {
                return Err(rejected(
                    "invalid",
                    json!({"need":["draft.version","draft.text"]}),
                ));
            };
            let context = Command {
                args: draft.clone(),
                ..command.clone()
            };
            let attachments = self
                .attachments(tx, &context)
                .map_err(|why| rejected("invalid_attachment", json!({"reason":why})))?;
            (version, text.to_owned(), attachments)
        } else {
            (
                self.core.draft.version,
                self.core.draft.text.clone(),
                self.core.draft.attachments.clone(),
            )
        };
        Ok(state::DraftRestore {
            version,
            text,
            attachments,
            device: command.device.clone(),
        })
    }

    pub(super) fn pin_return(
        &self,
        tx: &mut Tx<'_>,
        id: &str,
        restore: &state::DraftRestore,
        hold: bool,
    ) -> nd_store::Result<()> {
        self.reference_attachments(
            tx,
            RefOwner::Return {
                session: &self.id,
                id,
            },
            &restore.attachments,
            hold,
        )
    }

    pub(super) fn refill_draft(
        &mut self,
        tx: &mut Tx<'_>,
        id: &str,
        restore: &state::DraftRestore,
        messages: &[Message],
    ) -> nd_store::Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut text = restore.text.clone();
        let mut attachments = restore.attachments.clone();
        for message in messages {
            if !text.is_empty() && !message.text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&message.text);
            for attachment in &message.attachments {
                if !attachments.contains(attachment) {
                    attachments.push(attachment.clone());
                }
            }
        }
        let returned_id = format!("return/{id}");
        self.replace_or_save_draft(
            tx,
            &returned_id,
            &restore.device,
            restore.version,
            text,
            attachments,
        )?;
        Ok(())
    }

    pub(super) fn restore_withdrawn(
        &mut self,
        tx: &mut Tx<'_>,
        fx: &mut Effects,
        message: &Message,
    ) -> nd_store::Result<()> {
        if let Some(send) = &message.ticket {
            if let Some(mut row) = self.core.uncertain.remove(send) {
                row.outcome = None;
                self.core.outbox.insert(send.clone(), row);
            }
            self.record_outcome(
                tx,
                fx,
                send,
                Outcome::Refused {
                    refusal: Refusal::Withdrawn,
                },
            )?;
        }
        self.core.messages.remove(&message.id);
        self.show_message(tx, fx, message, "withdrawn", None)
    }

    pub(super) fn withdraw(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        fx: &mut Effects,
    ) -> nd_store::Result<Receipt> {
        let Some(id) = command.args["message"].as_str() else {
            return Ok(rejected("invalid", json!({"need":["message"]})));
        };
        let Some(message) = self.core.messages.get(id).cloned() else {
            return Ok(rejected("not_withdrawable", json!({"message":id})));
        };
        if self
            .core
            .outbox
            .values()
            .any(|r| matches!(&r.issuer, Issuer::Withdrawal { message: m, .. } if m.id == id))
        {
            return Ok(rejected("withdrawing", json!({"message":id})));
        }
        let restore = match self.return_draft(tx, command) {
            Ok(r) => r,
            Err(receipt) => return Ok(receipt),
        };
        let original = if let Some(send) = &message.ticket {
            let Some(original) = self.core.outbox.get(send).cloned() else {
                return Ok(rejected("not_withdrawable", json!({"message":id})));
            };
            Some(original)
        } else {
            None
        };
        if let Some(original) = original.filter(|row| row.handed) {
            let send = message.ticket.as_ref().expect("handed send has a ticket");
            self.pin_return(tx, &command.id, &restore, true)?;
            let ticket = Ticket(format!("control:{}", command.id));
            self.core.outbox.insert(
                ticket.clone(),
                OutRow {
                    issued: Issued {
                        ticket: ticket.clone(),
                        session: self.id.clone(),
                        write_gen: self.write_gen,
                    },
                    act: Act::Withdraw {
                        to: original.act.carrier().clone(),
                        send: send.clone(),
                    },
                    kind: original.kind,
                    issuer: Issuer::Withdrawal {
                        id: command.id.clone(),
                        message: message.clone(),
                        restore: restore.clone(),
                    },
                    display: None,
                    outcome: None,
                    handed: false,
                },
            );
            fx.hand.insert(0, ticket);
            self.show_message(tx, fx, &message, "withdrawing", None)?;
        } else {
            self.restore_withdrawn(tx, fx, &message)?;
            self.refill_draft(tx, &command.id, &restore, &[message])?;
        }
        Ok(Receipt::Done {
            value: json!({"withdrawal":command.id,"message":id}),
        })
    }

    pub(super) fn interrupt(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        fx: &mut Effects,
    ) -> nd_store::Result<Receipt> {
        let queued = match command.args["queued"].as_str().unwrap_or("keep") {
            "keep" => nd_backend::QueuedPolicy::Keep,
            "cancel" => nd_backend::QueuedPolicy::Cancel,
            _ => return Ok(rejected("invalid", json!({"queued":"keep or cancel"}))),
        };
        let carrier = self
            .core
            .current_carrier()
            .or_else(|| self.core.carriers.values().find(|c| c.alive))
            .cloned()
            .filter(|c| c.alive);
        let mut held = vec![];
        let restore = if queued == nd_backend::QueuedPolicy::Cancel {
            if carrier
                .as_ref()
                .is_some_and(|c| !c.interaction.cancel_queued)
            {
                return Ok(rejected(
                    "unsupported",
                    json!({"why":"后端未声明取消排队能力"}),
                ));
            }
            let restore = match self.return_draft(tx, command) {
                Ok(r) => r,
                Err(receipt) => return Ok(receipt),
            };
            held = self
                .core
                .messages
                .values()
                .filter(|m| m.ticket.is_none())
                .cloned()
                .collect();
            held.sort_by_key(|m| m.arrival);
            for message in &held {
                self.core.messages.remove(&message.id);
                self.show_message(tx, fx, message, "withdrawing", None)?;
            }
            Some(restore)
        } else {
            None
        };
        let Some(carrier) = carrier else {
            for message in &held {
                self.restore_withdrawn(tx, fx, message)?;
            }
            if let Some(restore) = restore {
                self.refill_draft(tx, &command.id, &restore, &held)?;
            }
            return Ok(Receipt::Done {
                value: json!({"idle":true}),
            });
        };
        if let Some(restore) = &restore {
            self.pin_return(tx, &command.id, restore, true)?;
        }
        let ticket = Ticket(format!("control:{}", command.id));
        self.core.outbox.insert(
            ticket.clone(),
            OutRow {
                issued: Issued {
                    ticket: ticket.clone(),
                    session: self.id.clone(),
                    write_gen: self.write_gen,
                },
                act: Act::Interrupt {
                    to: carrier.id,
                    queued,
                },
                kind: carrier.kind,
                issuer: Issuer::Control {
                    id: command.id.clone(),
                    restore,
                    held,
                },
                display: None,
                outcome: None,
                handed: false,
            },
        );
        fx.hand.insert(0, ticket);
        self.show(
            tx,
            fx,
            Shown::Control {
                id: command.id.clone(),
                state: "pending".into(),
                outcome: Value::Null,
            },
        )?;
        Ok(Receipt::Done {
            value: json!({"control":command.id}),
        })
    }

    pub(super) fn resend(
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

    pub(super) fn send(&mut self, tx: &mut Tx<'_>, command: &Command) -> nd_store::Result<Receipt> {
        let status = self.core.meta().status;
        if status == Status::Withdrawn
            || (status == Status::Partial && self.core.current_carrier().is_none())
        {
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
        self.core.meta.as_mut().unwrap().title.note_prompt(text);
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
        self.consume_draft_if_matches(tx, command, text, &attachments)?;
        // Done 只表示进了发送台；代持、写出、回显看这条消息的状态。
        Ok(Receipt::Done {
            value: json!({"message": command.id, "draft": self.core.draft}),
        })
    }
}
