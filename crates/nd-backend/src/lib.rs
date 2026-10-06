//! 后端端口：会话执行器对后端说话的唯一入口（规格「三个加深模块的契约」）。
//!
//! 这里只有后端无关的动作、结果、事实和路由，不做 I/O。真正的接缝是 [`BackendAdapter`]：
//! 生产实现是 Claude 适配（`nd-claude`），以后加 Codex；测试另有脚本化实现。
//! 动作与事实只加不改：执行器把它们存进发件箱和操作账，跨版本读回。
pub use nd_claims::{BackendKind, BackendSessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt, path::PathBuf, sync::Arc};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}
id!(
    /// 会话（侧栏里的一条）。
    SessionId
);
id!(
    /// 承载位：本会话要后端替它跑的一条对话的稳定编号，先后可由几个后端进程承载。
    CarrierId
);
id!(
    /// 后端进程编号：引擎铸造，`Open` 时交给端口；也是看守单元的名字后缀。
    RunId
);
id!(
    /// 票：发件箱里一条动作的编号。我方发起的原生编号（Claude 的 user uuid 等）由它确定性派生。
    Ticket
);

/// 由票确定性派生的 UUID 形状的原生编号：同一张票总得同一个值，不同的票（含另发尝试）不同。
pub fn native_uuid(ticket: &Ticket) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("nd-native:{}", ticket.0).as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// 交给端口的票：签发时（发件进发件箱的那个事务里）就定下，正常 `act` 与恢复时的 `adopt` 用同一份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issued {
    pub ticket: Ticket,
    pub session: SessionId,
    /// 签发它的会话执行器实例的写入代次。
    pub write_gen: u64,
}

/// 后端无关的动作。第 2 步先有承载位的一生（拉起、结束）和发送；其余动作随各自工单追加。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum Act {
    Open {
        carrier: CarrierId,
        run: RunId,
        spec: OpenSpec,
    },
    End {
        carrier: CarrierId,
        how: EndHow,
    },
    Invoke {
        to: CarrierId,
        invocation: Invocation,
    },
    Configure {
        to: CarrierId,
        setting: nd_wire::LiveSetting,
    },
    Send {
        to: CarrierId,
        msg: Msg,
    },
}
impl Act {
    pub fn carrier(&self) -> &CarrierId {
        match self {
            Act::Open { carrier, .. } | Act::End { carrier, .. } => carrier,
            Act::Send { to, .. } | Act::Configure { to, .. } | Act::Invoke { to, .. } => to,
        }
    }
    /// 恢复对账时证明没写出的票怎么办：要经独占登记放行的写类动作不补发（`Withhold`），
    /// 由引擎按当下事实重新放行另发；其余照写（`Resend`）。只由动作种类定。
    pub fn if_unsent(&self) -> IfUnsent {
        match self {
            Act::Open { .. } | Act::Send { .. } => IfUnsent::Withhold,
            Act::Invoke {
                invocation: Invocation::GenerateTitle { .. },
                ..
            } => IfUnsent::Withhold,
            Act::End { .. } | Act::Configure { .. } | Act::Invoke { .. } => IfUnsent::Resend,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfUnsent {
    Resend,
    Withhold,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSpec {
    #[serde(default)]
    pub live_settings: Vec<nd_wire::LiveSetting>,
    pub origin: Origin,
    pub profile: Profile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum Origin {
    /// 新建；Claude 用预定的后端会话 id（`--session-id`）。
    Fresh { id: BackendSessionId },
    /// 续接已有的后端会话：闲置回收后按需拉起。
    Resume { bs: BackendSessionId },
}
impl Origin {
    pub fn backend_session(&self) -> &BackendSessionId {
        match self {
            Origin::Fresh { id } => id,
            Origin::Resume { bs } => bs,
        }
    }
}

/// 后端种类、模型、权限模式、工作目录；跨后端换算经这个中立形状（ADR 0017）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub effort: Option<String>,
    pub kind: BackendKind,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndHow {
    /// 自然收尾：执行器确认收尾条件满足后才交；端口只做结束本身。
    Graceful,
    /// 用户结束会话、删除：端口先停回合与它持有的任务再结束。
    Finish,
    /// 降级：用户确认过会停掉旧进程的后台任务。
    Kill,
    /// 弃置本次新建、还没落定的目标。
    Discard,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Msg {
    #[serde(default)]
    pub attachments: Vec<nd_wire::Attachment>,
    pub text: String,
    pub intent: Intent,
}

/// 三种发送意图：并进当前回合（空闲就开新回合）、这一回合结束后再发、打断当前回合立即发。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    #[default]
    Fold,
    AfterTurn,
    Interrupting,
}

/// `act` 的同步结论：只校验、排队，不做 I/O。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "admit", rename_all = "snake_case")]
pub enum Admit {
    /// `may_be_unknown`：这张票会不会以「交付不明」结束。
    Accepted {
        may_be_unknown: bool,
    },
    Rejected {
        reject: Reject,
    },
}

/// 端口没接纳：端口里没有这张票的任何状态。`Busy` 不是终结结果，引擎用同一张票稍后再交。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reject", rename_all = "snake_case")]
pub enum Reject {
    Unsupported { why: String },
    NotAdopted,
    NoCarrier,
    Gone,
    Busy,
    Invalid { why: String },
    Conflict,
}

/// 票的终结结果；每张被接纳的票恰好一个。`Unknown` 也是终结。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Ok {
        done: Done,
    },
    Rejected {
        reject: Reject,
    },
    Refused {
        refusal: Refusal,
    },
    Failed {
        reason: String,
        partial: Vec<String>,
    },
    Unknown {
        evidence: String,
    },
}
impl Outcome {
    /// 原动作「可能已施加」：成功、交付不明、带已施加部分的失败。只有明确没做才是 false。
    pub fn possibly_applied(&self) -> bool {
        match self {
            Outcome::Ok { .. } | Outcome::Unknown { .. } => true,
            Outcome::Failed { partial, .. } => !partial.is_empty(),
            Outcome::Rejected { .. } | Outcome::Refused { .. } => false,
        }
    }
    pub fn failed(reason: impl Into<String>) -> Self {
        Outcome::Failed {
            reason: reason.into(),
            partial: vec![],
        }
    }
    pub fn reason(&self) -> String {
        match self {
            Outcome::Ok { .. } => "ok".into(),
            Outcome::Rejected { reject } => format!("rejected: {reject:?}"),
            Outcome::Refused { refusal } => format!("refused: {refusal:?}"),
            Outcome::Failed { reason, .. } => reason.clone(),
            Outcome::Unknown { evidence } => format!("交付不明：{evidence}"),
        }
    }
}

/// 接纳之后后端或端口明确没做。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    /// 恢复对账证明没写出，按 `IfUnsent::Withhold` 没有补发：引擎重新放行后另发。
    Withheld,
    /// 写出了，但端口证实没生效。
    Lost {
        evidence: String,
    },
    Other {
        why: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "done", rename_all = "snake_case")]
pub enum Done {
    Titled {
        title: Option<String>,
    },
    Configured {
        settings: Value,
    },
    Opened {
        bs: BackendSessionId,
        run: RunId,
        readiness: Readiness,
        /// 适配器在守护进程重启后接回这个进程要用的记录（Claude：拉起时的能力表），执行器原样保存。
        adopt: Value,
    },
    Ended {
        code: Option<i32>,
    },
    /// 送达：后端回显了这条输入的原生编号（Claude：带原 uuid 的 `isReplay` 回显）。
    Landed {
        native: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "readiness", rename_all = "snake_case")]
pub enum Readiness {
    Full,
    /// 只能聊天：mod 没报到等原因，会话头写明。
    ChatOnly {
        why: String,
    },
}

/// 后端事实。`key` 是原生去重键：同一个键再来一次（重启后重放流水）不重复入账。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub key: String,
    pub body: FactBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum FactBody {
    TitleChanged {
        title: String,
    },
    /// 此承载位的 hello、流水追平和未结票对账完成；本代恢复闸门据此放行。
    Recovered,
    /// Unknown 之后的新证据；不产生第二个终结结果，由原签发者更新当前结论。
    Clarified {
        ticket: Ticket,
        outcome: Outcome,
    },
    /// 一张票的终结结果。
    Done {
        ticket: Ticket,
        outcome: Outcome,
    },
    /// 持久的中间证据，不结票：已写给后端进程。`native` 是写出时用的原生编号。
    Written {
        ticket: Ticket,
        native: String,
    },
    TurnStarted,
    /// 已确认的实际回合与用户输入原生位置；不代替 Landed 的送达证据。
    TurnMapped {
        turn: String,
        natives: Vec<String>,
        complete: bool,
        last_assistant: Option<String>,
    },
    /// 一个回合结束。`ok` 已核 `is_error`；不代表模型回答了哪条消息，也不结票。
    TurnEnded {
        ok: bool,
        subtype: String,
        error: Option<String>,
    },
    /// 完成的内容条目；之前的增量由它整体替换，不追加。
    Item {
        item: Item,
    },
    /// 后端进程退出，且独占登记已按看守的观察记下它离开。
    Exited {
        run: RunId,
        code: Option<i32>,
    },
    /// 后台任务能不能收尾：拿不到可靠的表时是 `Unknown`，不当成空。
    Tasks {
        drain: Drain,
    },
    /// 流水真丢了重建不出的行（`lost`），或按策略丢了增量。
    Gap {
        lost: bool,
    },
    /// 后端等待回答的请求（审批、提问等）；第 2 步只显示，不作答。
    Asked {
        id: String,
        kind: String,
        raw: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// 内容块身份：（API 消息 id，块序号），或工具结果的工具调用 id。
    pub id: String,
    pub kind: ItemKind,
    pub text: String,
    #[serde(default)]
    pub raw: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Text,
    Thinking,
    ToolUse,
    ToolResult,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "drain", rename_all = "snake_case")]
pub enum Drain {
    Busy,
    Drained,
    Unknown { why: String },
}

/// 纯增量：不开事务，只累加进内存里的进行中条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "live", rename_all = "snake_case")]
pub enum Live {
    Delta {
        item: String,
        kind: ItemKind,
        text: String,
    },
}

/// 不透明的检查点：适配器自己的流水位置等，执行器原样随批次提交、恢复时交回。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Checkpoint(pub Value);

/// 一个承载位一批：同一流水行推出的事实在同一批，检查点单调。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub carrier: CarrierId,
    pub facts: Vec<Fact>,
    pub live: Vec<Live>,
    pub checkpoint: Option<Checkpoint>,
}

/// 批次的事实和检查点已提交：据此给看守确认。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub session: SessionId,
    pub carrier: CarrierId,
    pub checkpoint: Checkpoint,
}

/// 稳定收件地址：适配器把批次交到这里，不反向调用会话。
pub type Inbox = tokio::sync::mpsc::Sender<Batch>;

/// 会话装载时（含守护进程重启后）交给适配器：收件地址、承载位、未结的票。
#[derive(Clone, Debug)]
pub struct AdoptPart {
    pub session: SessionId,
    pub inbox: Inbox,
    pub carriers: Vec<CarrierRecord>,
    pub pending: Vec<PendingTicket>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarrierRecord {
    pub carrier: CarrierId,
    pub kind: BackendKind,
    /// 当前承载它的后端进程；没有进程时为 None。
    pub run: Option<RunId>,
    pub bs: Option<BackendSessionId>,
    /// `Done::Opened.adopt` 原样交回。
    pub adopt: Value,
    pub checkpoint: Option<Checkpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTicket {
    pub issued: Issued,
    pub act: Act,
}

/// 真端口。生产：Claude 适配；测试：脚本化适配器。
pub trait BackendAdapter: Send + Sync {
    /// 会话外的只读查询；外部进程的生命周期关在适配器里。
    fn models(&self, _cwd: std::path::PathBuf) -> ModelQuery<'_> {
        Box::pin(async { Err("这个后端没有提供模型列表".into()) })
    }
    fn kind(&self) -> BackendKind;
    /// `act` 之前每个会话调一次；之后适配器对账未结票、从检查点接着读。
    fn adopt(&self, part: AdoptPart);
    /// 交一张已提交的票。只校验、排队，不做 I/O、不阻塞执行器。
    fn act(&self, issued: Issued, act: Act) -> Admit;
    /// 批次已提交：据此给看守 ack。
    fn committed(&self, ack: Ack);
    /// 会话卸下：适配器丢掉收件地址，不结束后端进程。
    fn release(&self, _session: &SessionId) {}
}

pub type ModelQuery<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<nd_wire::Model>, String>> + Send + 'a>,
>;

/// 会话执行器看到的全部后端：按后端种类路由到适配器。具体类型，只有一份实现。
#[derive(Clone, Default)]
pub struct Backends {
    adapters: BTreeMap<String, Arc<dyn BackendAdapter>>,
}
fn kind_key(kind: &BackendKind) -> String {
    format!("{kind:?}")
}
impl Backends {
    pub async fn models(
        &self,
        backend: &str,
        cwd: std::path::PathBuf,
    ) -> Result<Vec<nd_wire::Model>, String> {
        let kind = match backend {
            "claude" => BackendKind::Claude,
            "codex" => BackendKind::Codex,
            _ => return Err("没有这个后端".into()),
        };
        self.get(&kind).ok_or("后端未配置")?.models(cwd).await
    }
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(mut self, adapter: Arc<dyn BackendAdapter>) -> Self {
        self.adapters.insert(kind_key(&adapter.kind()), adapter);
        self
    }
    pub fn supports(&self, kind: &BackendKind) -> bool {
        self.adapters.contains_key(&kind_key(kind))
    }
    fn get(&self, kind: &BackendKind) -> Option<&Arc<dyn BackendAdapter>> {
        self.adapters.get(&kind_key(kind))
    }
    /// 每个种类的适配器各得自己那几个承载位。
    pub fn adopt(&self, part: AdoptPart, kind_of: impl Fn(&CarrierId) -> Option<BackendKind>) {
        for (key, adapter) in &self.adapters {
            let carriers: Vec<_> = part
                .carriers
                .iter()
                .filter(|c| kind_key(&c.kind) == *key)
                .cloned()
                .collect();
            let pending: Vec<_> = part
                .pending
                .iter()
                .filter(|p| kind_of(p.act.carrier()).is_some_and(|k| kind_key(&k) == *key))
                .cloned()
                .collect();
            adapter.adopt(AdoptPart {
                session: part.session.clone(),
                inbox: part.inbox.clone(),
                carriers,
                pending,
            });
        }
    }
    pub fn act(&self, kind: &BackendKind, issued: Issued, act: Act) -> Admit {
        match self.get(kind) {
            Some(adapter) => adapter.act(issued, act),
            None => Admit::Rejected {
                reject: Reject::Unsupported {
                    why: format!("没有 {kind:?} 后端"),
                },
            },
        }
    }
    pub fn committed(&self, kind: &BackendKind, ack: Ack) {
        if let Some(adapter) = self.get(kind) {
            adapter.committed(ack);
        }
    }
    pub fn release(&self, session: &SessionId) {
        for adapter in self.adapters.values() {
            adapter.release(session);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "invoke", rename_all = "snake_case")]
pub enum Invocation {
    Title { title: String },
    GenerateTitle { description: String },
}
