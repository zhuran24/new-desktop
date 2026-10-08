//! nd-wire 的唯一类型来源；枚举型能力名称用开放字符串保留未来值。
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

mod settings;
pub use settings::{EffectiveSettings, LiveSettings, SettingCaps};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_ATTACHMENT_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 8;
pub const MAX_MESSAGE_ATTACHMENT_BYTES: u64 = 16 * 1024 * 1024;

/// 会话持久草稿；光标、选区和输入法组词不在此协议中。
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Draft {
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub version: u64,
    pub text: String,
    pub device: String,
    #[serde(default)]
    pub saved: Vec<SavedDraft>,
}

/// 版本比较落败的原文；id 是原编辑命令的 id，重试不重复另存。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SavedDraft {
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub id: String,
    pub base_version: u64,
    pub text: String,
    pub device: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DraftUpdated {
    pub draft: Draft,
    /// None 为替换当前稿；Some 为另存稿的 id。两种都是已持久化的 Done。
    pub saved: Option<String>,
}

/// `session.draft.update` 的参数；前置版本放在 Command.expect。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DraftUpdate {
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub session: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DraftExpected {
    pub draft_version: u64,
}

/// `session.shell` 的参数：`!` 模式在后端进程里跑一条 shell 命令，命令和输出追加进对话，本身不起回合。
/// `input` 是输入框原文（含开头的 `!`）：等于当前稿且 `expect.draft_version` 对得上时同事务清稿。
/// 收据等动作有结果：`Done.value` 是 [`Invoked`]。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ShellArgs {
    pub session: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

/// `session.subtask` 的参数：派 fork 型子代理，带着当前上下文分出一件子任务。`input` 同 [`ShellArgs`]。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SubtaskArgs {
    pub session: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

/// `session.compact` 的参数：在一条人类提示上「从这里总结」（`from`）或「总结到这里」（`up_to`）。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CompactArgs {
    pub session: String,
    /// 提示的消息 id（`prompt/<消息 id>` 条目的 `message`）。
    pub message: String,
    pub scope: CompactScope,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactScope {
    /// 从所选提示（含）到末尾；成功后提示原文回到输入框。
    From,
    /// 从开头到所选提示（不含）；成功后输入框留空。
    UpTo,
}

/// `session.shell`、`session.subtask`、`session.compact` 的 `Done.value`。字段按种类出现。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Invoked {
    /// 起它的命令 id，也是会话流里 `invoke/<id>` 条目的 id。
    pub invoke: String,
    /// 结果落定那一刻的当前草稿（总结的回填已在里面）。
    pub draft: Draft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// `!` 的命令和输出已追加进对话（模型下一次请求读得到）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appended: Option<bool>,
    /// fork 型子代理的 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// 总结回填时另存的稿（被替换的旧稿、或基准过时没能回填的原文）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved: Vec<String>,
}

/// 后端实时给出的模型选项；value 原样用于 session.create，不从显示名称推导。
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default)]
pub struct Model {
    pub value: String,
    pub label: String,
    pub description: String,
    pub disabled: bool,
    pub resolved_model: Option<String>,
    pub effort_levels: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Fallback {
    pub title: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Item {
    pub id: String,
    pub namespace: String,
    pub kind: String,
    pub data: Value,
    pub fallback: Fallback,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Snapshot {
    pub stream: String,
    pub epoch: String,
    pub cursor: u64,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}
impl Snapshot {
    pub fn position(&self) -> Cursor {
        Cursor {
            epoch: self.epoch.clone(),
            seq: self.cursor,
        }
    }
}
impl Cursor {
    pub fn is_followed_by(&self, event: &Event) -> bool {
        self.epoch == event.epoch && self.seq.checked_add(1) == Some(event.cursor)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Models {
        id: u64,
        backend: String,
        cwd: String,
    },
    Hello {
        version: u32,
        namespaces: BTreeMap<String, u32>,
    },
    Subscribe {
        stream: String,
        #[serde(default)]
        since: Option<Cursor>,
    },
    Get {
        id: u64,
        res: String,
        page: PageReq,
    },
    Command {
        id: u64,
        name: String,
    },
    Execute {
        id: u64,
        command: Command,
    },
    Receipt {
        id: u64,
        command_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_hash: Option<String>,
    },
    Bye,
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub stream: String,
    pub epoch: String,
    pub cursor: u64,
    pub upsert: Vec<Item>,
    pub remove: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        version: u32,
        epoch: String,
        namespaces: BTreeMap<String, u32>,
    },
    Snapshot {
        snapshot: Snapshot,
    },
    Event {
        event: Event,
    },
    Resumed {
        stream: String,
        epoch: String,
        cursor: u64,
    },
    Reply {
        id: u64,
        value: Value,
        error: Option<String>,
    },
    CommandReply {
        id: u64,
        result: CommandReply,
    },
    ReceiptReply {
        id: u64,
        result: ReceiptLookup,
    },
    Error {
        code: String,
        message: String,
    },
    Bye {
        resume: bool,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PageReq {
    pub before: Option<String>,
    pub limit: u32,
    /// 按稳定轮 id 定位；与 before/after 互斥。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub around: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}
impl Default for PageReq {
    fn default() -> Self {
        Self {
            before: None,
            limit: 60,
            around: None,
            after: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    pub items: Vec<Item>,
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// 本页和会话事件流的同一观察点；界面据此合并先到的更新。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Cursor>,
}

/// id 在守护进程内全局唯一；device 是来源标识，本机权限仍由 socket uid 决定。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Command {
    pub id: String,
    pub device: String,
    pub name: String,
    pub args: Value,
    pub expect: Value,
}

/// 提交时的永久结论，后续操作进度不能改写已签发的收据。
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Receipt {
    Done {
        value: Value,
    },
    /// 起了一个要等的操作。`stream`：能看到它进度的流（例如新建会话的 `session/<id>`）；只加不改的可选字段。
    Accepted {
        op: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
    },
    Rejected {
        code: String,
        now: Value,
    },
    Unknown {
        now: Value,
    },
    /// A newer peer returned a status this version does not interpret.
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableCode {
    Recovering,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandReply {
    Receipt {
        receipt: Receipt,
    },
    Conflict,
    Expired,
    /// 明确未受理、没有新收据，只有这一种情况可自动同 id 重试。
    Unavailable {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<UnavailableCode>,
    },
    /// 仅同步副本产生：断线后查不到收据，不能自动重发正文。
    DeliveryUnknown,
    /// A newer peer returned a status this version does not interpret.
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReceiptLookup {
    Conflict,
    Found {
        receipt: Receipt,
    },
    Missing,
    Expired,
    Unavailable {
        reason: String,
    },
    /// A newer peer returned a status this version does not interpret.
    #[serde(other)]
    Other,
}

impl Command {
    /// 完整已知信封的 SHA-256；递归排序对象键，不把 JSON 键顺序当成不同命令。
    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        fn sorted(value: Value) -> Value {
            match value {
                Value::Object(map) => Value::Object(
                    map.into_iter()
                        .collect::<BTreeMap<_, _>>()
                        .into_iter()
                        .map(|(k, v)| (k, sorted(v)))
                        .collect(),
                ),
                Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
                other => other,
            }
        }
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&sorted(serde_json::to_value(self).unwrap())).unwrap()
            )
        )
    }
}
/// 附件正文经鉴权 HTTP GET/PUT，消息和草稿只携带内容寻址引用。
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Attachment {
    pub blob: nd_id::BlobId,
    pub name: String,
    pub media_type: String,
    pub size: u64,
}

impl Attachment {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 255 || self.name.chars().any(char::is_control)
        {
            return Err("附件名称为空、过长或含控制字符".into());
        }
        if !matches!(
            self.media_type.as_str(),
            "image/png"
                | "image/jpeg"
                | "image/gif"
                | "image/webp"
                | "text/plain"
                | "application/pdf"
        ) {
            return Err("不支持此附件类型".into());
        }
        if self.size == 0 || self.size > MAX_ATTACHMENT_BYTES {
            return Err("单个附件须为 1 字节至 5 MiB".into());
        }
        Ok(())
    }
}

/// 会话运行时设置；一次命令只改一项，结果以会话头的回读为准。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveSetting {
    Model(String),
    Effort(String),
    Ultracode(bool),
    PermissionMode(String),
}

/// session.configure 的参数。只修改一个会话运行时设置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigureSession {
    pub session: String,
    pub setting: LiveSetting,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenameSession {
    pub session: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsExpected {
    pub settings_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TitleExpected {
    pub title_revision: u64,
}

mod item_state;
pub use item_state::{ControlState, PromptState};

pub use nd_id::BlobId;
