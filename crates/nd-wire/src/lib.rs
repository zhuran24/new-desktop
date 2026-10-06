//! nd-wire 的唯一类型来源；枚举型能力名称用开放字符串保留未来值。
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 1;

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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
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
}
impl Default for PageReq {
    fn default() -> Self {
        Self {
            before: None,
            limit: 100,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    pub items: Vec<Item>,
    pub next: Option<String>,
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
    Done { value: Value },
    Accepted { op: String },
    Rejected { code: String, now: Value },
    Unknown { now: Value },
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
    },
    /// 仅同步副本产生：断线后查不到收据，不能自动重发正文。
    DeliveryUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReceiptLookup {
    Conflict,
    Found { receipt: Receipt },
    Missing,
    Expired,
    Unavailable { reason: String },
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
