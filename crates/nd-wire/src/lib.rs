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
