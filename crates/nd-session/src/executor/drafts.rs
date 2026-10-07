use super::*;

/// 持久引用的命名规则；同一类型同时用于取得和释放，不在调用点拼字符串。
pub(super) enum RefOwner<'a> {
    Message {
        session: &'a SessionId,
        command: &'a str,
    },
    Draft {
        session: &'a SessionId,
    },
    SavedDraft {
        session: &'a SessionId,
        command: &'a str,
    },
    Return {
        session: &'a SessionId,
        id: &'a str,
    },
}
impl std::fmt::Display for RefOwner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message { session, command } => write!(f, "message/{session}/{command}"),
            Self::Draft { session } => write!(f, "draft/{session}"),
            Self::SavedDraft { session, command } => write!(f, "draft-saved/{session}/{command}"),
            Self::Return { session, id } => write!(f, "return/{session}/{id}"),
        }
    }
}
impl Executor {
    pub(super) fn reference_attachments(
        &self,
        tx: &mut Tx<'_>,
        owner: RefOwner<'_>,
        attachments: &[nd_wire::Attachment],
        hold: bool,
    ) -> nd_store::Result<()> {
        let owner = owner.to_string();
        for attachment in attachments {
            if hold {
                self.deps.blobs.hold(tx, &attachment.blob, &owner)?;
            } else {
                self.deps.blobs.release(tx, &attachment.blob, &owner)?;
            }
        }
        Ok(())
    }
    pub(super) fn replace_or_save_draft(
        &mut self,
        tx: &mut Tx<'_>,
        id: &str,
        device: &str,
        base: u64,
        text: String,
        attachments: Vec<nd_wire::Attachment>,
    ) -> nd_store::Result<nd_wire::DraftUpdated> {
        let owner = if base == self.core.draft.version {
            self.reference_attachments(
                tx,
                RefOwner::Draft { session: &self.id },
                &self.core.draft.attachments,
                false,
            )?;
            RefOwner::Draft { session: &self.id }
        } else {
            RefOwner::SavedDraft {
                session: &self.id,
                command: id,
            }
        };
        self.reference_attachments(tx, owner, &attachments, true)?;
        Ok(self.core.update_draft(id, device, base, text, attachments))
    }
    pub(super) fn consume_draft_if_matches(
        &mut self,
        tx: &mut Tx<'_>,
        command: &Command,
        text: &str,
        attachments: &[nd_wire::Attachment],
    ) -> nd_store::Result<bool> {
        if command.expect["draft_version"].as_u64() != Some(self.core.draft.version)
            || self.core.draft.text != text
            || self.core.draft.attachments != attachments
        {
            return Ok(false);
        }
        self.replace_or_save_draft(
            tx,
            &command.id,
            &command.device,
            self.core.draft.version,
            String::new(),
            vec![],
        )?;
        Ok(true)
    }
}
