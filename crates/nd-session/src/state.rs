//! 会话执行器的持久状态：一个小的核心记录（JSON）加显示缓存表。
//!
//! 核心保留谱系索引、承载位、未结的票、进行中的操作和发送台里没结论的消息；
//! 对话正文进显示缓存。一个输入一个事务，整份核心随事务写回。
use crate::journal::OpRecord;
use nd_backend::{
    Act, BackendKind, BackendSessionId, CarrierId, Checkpoint, Drain, Intent, Issued, Outcome,
    Readiness, RunId, SessionId, Ticket,
};
use nd_store::{OptionalExtension, Tx, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// 创建中：种子操作还没落定，侧栏标「准备中」。
    Preparing,
    Active,
    /// 创建失败但做过不可逆步骤：会话保留，标「部分完成」。
    Partial,
    /// 创建失败、没做过不可逆步骤：撤掉，不再出现在侧栏。
    Withdrawn,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Preparing => "preparing",
            Status::Active => "active",
            Status::Partial => "partial",
            Status::Withdrawn => "withdrawn",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    Summary,
    Ai,
    Manual,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTitle {
    #[serde(rename = "title")]
    pub text: Option<String>,
    #[serde(rename = "title_source")]
    pub source: Option<TitleSource>,
    #[serde(rename = "title_seed")]
    pub seed: Option<String>,
    #[serde(rename = "title_attempted")]
    pub attempted: bool,
    #[serde(rename = "title_revision")]
    pub revision: u64,
}
impl SessionTitle {
    pub fn new(text: &str) -> Self {
        let mut title = Self {
            text: Some(text.trim().chars().take(60).collect()),
            source: Some(TitleSource::Summary),
            ..Self::default()
        };
        title.note_prompt(text);
        title
    }
    pub fn may_auto_generate(&self) -> bool {
        !self.attempted && self.seed.is_some() && self.source != Some(TitleSource::Manual)
    }
    pub fn note_prompt(&mut self, text: &str) {
        if !self.attempted
            && self.seed.is_none()
            && self.source != Some(TitleSource::Manual)
            && text.trim().chars().count() >= 10
            && !text.trim_start().starts_with(['!', '/'])
        {
            self.seed = Some(text.to_owned());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub id: SessionId,
    pub status: Status,
    /// 建它的那条命令的 id。
    pub created_by: String,
    pub cwd: PathBuf,
    pub kind: BackendKind,
    pub model: Option<String>,
    /// 已确认接受的 effort 意图；模型暂不支持时仍保留，供续接和以后换模型。
    #[serde(default)]
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    #[serde(default, deserialize_with = "read_settings")]
    pub settings: nd_backend::LiveSettings,
    #[serde(flatten)]
    pub title: SessionTitle,
    #[serde(default)]
    pub settings_revision: u64,
    /// 撤掉或部分完成的原因。
    pub note: Option<String>,
    /// 部分完成时已经做过（或可能做过）的不可逆步骤。
    pub irreversible: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Carrier {
    pub id: CarrierId,
    pub kind: BackendKind,
    pub bs: BackendSessionId,
    /// 正承载它的后端进程；没有活进程时为 None。
    pub run: Option<RunId>,
    pub alive: bool,
    pub readiness: Option<Readiness>,
    #[serde(default)]
    pub interaction: nd_backend::InteractionCaps,
    /// `Done::Opened.adopt`：适配器在守护进程重启后接回要用。
    pub adopt: Value,
    pub checkpoint: Option<Checkpoint>,
    /// 后台任务能否收尾；没报过就是 Unknown，不当成空。
    pub drain: Drain,
    pub turn_running: bool,
    #[serde(default)]
    pub turn: Option<nd_backend::TurnRef>,
    pub turns: u64,
    /// 端口报的能力表（拉起时、能力变了时）；旧记录没有时为空。
    #[serde(default)]
    pub features: Vec<nd_backend::Feature>,
}
impl Carrier {
    /// 某项能力不可用的原因；能力表里没有这一项时按可用处理（由端口在执行时再核）。
    pub fn unavailable(&self, feature: &str) -> Option<String> {
        if let Some(Readiness::ChatOnly { why }) = &self.readiness
            && self.features.is_empty()
        {
            return Some(why.clone());
        }
        self.features
            .iter()
            .find(|f| f.id == feature && !f.available)
            .map(|f| f.why.clone().unwrap_or_else(|| "后端进程不提供".into()))
    }
}

/// 发件箱的一行：签发它的是操作账里的一个键，或发送台里的一条消息。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutRow {
    pub issued: Issued,
    pub act: Act,
    pub kind: BackendKind,
    pub issuer: Issuer,
    /// 显示这张票（发送）的条目 id。
    pub display: Option<String>,
    pub outcome: Option<Outcome>,
    /// 已经交给端口（提交之后 `act`）。恢复时没交出的照样在 `adopt` 里对账。
    pub handed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Issuer {
    Op {
        op: String,
        key: String,
    },
    Message {
        id: String,
        #[serde(default)]
        arrival: u64,
    },
    /// 经后端进程做的一件事（总结、`!`、派 fork 型子代理）；`id` 是起它的命令 id。
    Invoke {
        id: String,
    },
    Control {
        id: String,
        #[serde(default)]
        restore: Option<DraftRestore>,
        #[serde(default)]
        held: Vec<Message>,
    },
    Withdrawal {
        id: String,
        message: Message,
        #[serde(default)]
        restore: DraftRestore,
    },
}

/// 控制操作开始时看到的草稿基准；完成时版本已变化就走 #16 的另存稿。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRestore {
    #[serde(default)]
    pub attachments: Vec<nd_wire::Attachment>,
    pub version: u64,
    pub text: String,
    pub device: String,
}

/// 发送台共同的签票状态；平铺序列化，兼容既有持久核心。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    pub ticket: Option<Ticket>,
    pub attempt: u32,
    pub waiting: Option<String>,
    pub arrival: u64,
}
impl QueueState {
    pub fn retry(&mut self) {
        self.ticket = None;
        self.attempt += 1;
    }
    pub fn sent(&mut self, ticket: Ticket) {
        self.ticket = Some(ticket);
        self.waiting = None;
    }
}

/// 发送台里还没结论的一条消息。界面上始终是这一条，另发尝试不换消息。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub attachments: Vec<nd_wire::Attachment>,
    pub id: String,
    pub text: String,
    pub intent: Intent,
    /// 当前尝试的票；None 表示还在发送台里（代持或等独占）。
    #[serde(flatten)]
    pub queue: QueueState,
}

/// 收据等动作有结果的一件事（规格「收据时点」）：命令受理时记下意图，结果到了才落收据。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Invoke {
    /// 起它的命令 id，也是界面条目 `invoke/<id>` 的 id。
    pub id: String,
    /// 命令的内容散列：同 id 不同内容回 conflict。
    pub digest: String,
    pub device: String,
    pub invocation: nd_backend::Invocation,
    /// 总结所选提示的消息 id。
    #[serde(default)]
    pub message: Option<String>,
    /// 「从这里总结」成功后放回输入框的原文与附件。
    #[serde(default)]
    pub restore: Option<(String, Vec<nd_wire::Attachment>)>,
    /// 受理时的草稿版本：回填以它为基准，之后的编辑不被覆盖。
    pub draft_base: u64,
    /// 总结成功后不再是 CLI 对话行的提示（受理时算出）。
    #[serde(default)]
    pub covers: Vec<String>,
    #[serde(flatten)]
    pub queue: QueueState,
}
impl Invoke {
    pub fn kind(&self) -> &'static str {
        match self.invocation {
            nd_backend::Invocation::Compact { .. } => "compact",
            nd_backend::Invocation::Shell { .. } => "shell",
            nd_backend::Invocation::ForkAgent { .. } => "subtask",
            nd_backend::Invocation::Title { .. } | nd_backend::Invocation::GenerateTitle { .. } => {
                "title"
            }
        }
    }
    /// 端口能力表里对应的那一项。
    pub fn feature(&self) -> &'static str {
        match self.invocation {
            nd_backend::Invocation::Compact { .. } => "summarize",
            nd_backend::Invocation::Shell { .. } => "bang_mode",
            nd_backend::Invocation::ForkAgent { .. } => "fork_subagent",
            nd_backend::Invocation::Title { .. } | nd_backend::Invocation::GenerateTitle { .. } => {
                "title"
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Core {
    #[serde(default)]
    pub draft: nd_wire::Draft,
    pub meta: Option<Meta>,
    pub current: Option<CarrierId>,
    #[serde(default)]
    pub lineage: crate::lineage::Lineage,
    pub carriers: BTreeMap<CarrierId, Carrier>,
    pub ops: BTreeMap<String, OpRecord>,
    pub outbox: BTreeMap<Ticket, OutRow>,
    #[serde(default)]
    pub uncertain: BTreeMap<Ticket, OutRow>,
    #[serde(default)]
    pub undelivered: BTreeMap<String, nd_backend::Msg>,
    pub messages: BTreeMap<String, Message>,
    pub next_op: u64,
    pub arrivals: u64,
    /// 等结果的总结、`!` 命令、fork 型子代理，按命令 id。
    #[serde(default)]
    pub invokes: BTreeMap<String, Invoke>,
    /// 已被总结、不再是 CLI 对话行的提示（消息 id）。
    #[serde(default)]
    pub summarized: BTreeSet<String>,
}
impl Core {
    pub fn meta(&self) -> &Meta {
        self.meta.as_ref().expect("session row exists")
    }
    pub fn current_carrier(&self) -> Option<&Carrier> {
        self.current.as_ref().and_then(|c| self.carriers.get(c))
    }
    /// 执行器事务内的编辑/回填入口；调用方用收据或操作账保证同 id 只落定一次。
    /// 撤回、总结、回退可在处理其完成事实的同一事务内复用，不另发清稿命令。
    pub fn update_draft(
        &mut self,
        id: &str,
        device: &str,
        base: u64,
        text: String,
        attachments: Vec<nd_wire::Attachment>,
    ) -> nd_wire::DraftUpdated {
        let saved = if base != self.draft.version {
            self.draft.saved.push(nd_wire::SavedDraft {
                id: id.into(),
                base_version: base,
                attachments,
                text,
                device: device.into(),
            });
            Some(id.into())
        } else {
            self.draft.version += 1;
            self.draft.text = text;
            self.draft.attachments = attachments;
            self.draft.device = device.into();
            None
        };
        nd_wire::DraftUpdated {
            draft: self.draft.clone(),
            saved,
        }
    }
}

pub fn migrate(tx: &mut Tx<'_>) -> nd_store::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY, gen INTEGER NOT NULL, status TEXT NOT NULL,
            created_by TEXT NOT NULL, core TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS session_items (
            session TEXT NOT NULL, id TEXT NOT NULL, seq INTEGER NOT NULL, body TEXT NOT NULL,
            PRIMARY KEY(session, id));",
    )?;
    Ok(())
}

fn corrupt(e: impl std::fmt::Display) -> nd_store::Error {
    nd_store::Error::Aborted(format!("session state: {e}"))
}

/// 读出的会话：写入代次、核心。
pub fn load(tx: &Tx<'_>, id: &SessionId) -> nd_store::Result<Option<(u64, Core)>> {
    let row: Option<(u64, String)> = tx
        .query_row(
            "SELECT gen, core FROM sessions WHERE id=?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(write_gen, core)| Ok((write_gen, serde_json::from_str(&core).map_err(corrupt)?)))
        .transpose()
}

pub fn insert(tx: &Tx<'_>, core: &Core, write_gen: u64) -> nd_store::Result<()> {
    let meta = core.meta();
    tx.execute(
        "INSERT INTO sessions(id, gen, status, created_by, core) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            meta.id.as_str(),
            write_gen,
            meta.status.as_str(),
            meta.created_by,
            serde_json::to_string(core).map_err(corrupt)?
        ],
    )?;
    Ok(())
}

pub fn save(tx: &Tx<'_>, core: &Core) -> nd_store::Result<()> {
    let meta = core.meta();
    tx.execute(
        "UPDATE sessions SET status=?2, core=?3 WHERE id=?1",
        params![
            meta.id.as_str(),
            meta.status.as_str(),
            serde_json::to_string(core).map_err(corrupt)?
        ],
    )?;
    Ok(())
}

/// 写入代次加一（G8）：之后旧实例的提交得 `Fenced`。
pub fn bump_gen(tx: &Tx<'_>, id: &SessionId) -> nd_store::Result<Option<u64>> {
    tx.execute("UPDATE sessions SET gen=gen+1 WHERE id=?1", [id.as_str()])?;
    tx.query_row("SELECT gen FROM sessions WHERE id=?1", [id.as_str()], |r| {
        r.get(0)
    })
    .optional()
    .map_err(Into::into)
}

pub fn write_gen(tx: &Tx<'_>, id: &SessionId) -> nd_store::Result<Option<u64>> {
    tx.query_row("SELECT gen FROM sessions WHERE id=?1", [id.as_str()], |r| {
        r.get(0)
    })
    .optional()
    .map_err(Into::into)
}

pub fn put_item(
    tx: &Tx<'_>,
    session: &SessionId,
    item: &nd_wire::Item,
    seq: u64,
) -> nd_store::Result<()> {
    tx.execute(
        "INSERT INTO session_items(session, id, seq, body) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session, id) DO UPDATE SET seq=excluded.seq, body=excluded.body",
        params![
            session.as_str(),
            item.id,
            seq,
            serde_json::to_string(item).map_err(corrupt)?
        ],
    )?;
    Ok(())
}

pub fn items(
    store: &nd_store::Store,
    session: &SessionId,
) -> nd_store::Result<Vec<(u64, nd_wire::Item)>> {
    let db = store.read()?;
    let mut stmt =
        db.prepare("SELECT seq, body FROM session_items WHERE session=?1 ORDER BY seq")?;
    let rows = stmt.query_map([session.as_str()], |r| {
        Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = vec![];
    for row in rows {
        let (seq, body) = row?;
        out.push((seq, serde_json::from_str(&body).map_err(corrupt)?));
    }
    Ok(out)
}

/// 守护进程启动时要先装载的会话：有活进程、有进行中的操作或有未结的票。
pub fn needing_recovery(store: &nd_store::Store) -> nd_store::Result<Vec<SessionId>> {
    let db = store.read()?;
    let mut stmt = db.prepare("SELECT id, core FROM sessions WHERE status<>'withdrawn'")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = vec![];
    for row in rows {
        let (id, core) = row?;
        let core: Core = serde_json::from_str(&core).map_err(corrupt)?;
        if !core.ops.is_empty()
            || !core.outbox.is_empty()
            || !core.uncertain.is_empty()
            || !core.messages.is_empty()
            || !core.invokes.is_empty()
            || core.carriers.values().any(|c| c.run.is_some())
        {
            out.push(SessionId(id));
        }
    }
    Ok(out)
}

/// 侧栏列表：没撤掉的会话。
pub fn listed(store: &nd_store::Store) -> nd_store::Result<Vec<Core>> {
    let db = store.read()?;
    let mut stmt =
        db.prepare("SELECT core FROM sessions WHERE status<>'withdrawn' ORDER BY rowid")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = vec![];
    for row in rows {
        out.push(serde_json::from_str(&row?).map_err(corrupt)?);
    }
    Ok(out)
}

fn read_settings<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<nd_backend::LiveSettings, D::Error> {
    Ok(Option::<nd_backend::LiveSettings>::deserialize(de)?.unwrap_or_default())
}

/// Gone 流水回收的持久引用屏障，包括未结和交付不明的 Open 票。
pub fn referenced_runs(
    store: &nd_store::Store,
) -> nd_store::Result<std::collections::BTreeSet<String>> {
    let db = store.read()?;
    let mut stmt = db.prepare("SELECT core FROM sessions")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut runs = std::collections::BTreeSet::new();
    for row in rows {
        let core: Core = serde_json::from_str(&row?).map_err(corrupt)?;
        runs.extend(
            core.carriers
                .values()
                .filter_map(|c| c.run.as_ref())
                .map(|r| r.0.clone()),
        );
        for row in core.outbox.values().chain(core.uncertain.values()) {
            if let nd_backend::Act::Open { run, .. } = &row.act {
                runs.insert(run.0.clone());
            }
        }
    }
    Ok(runs)
}
