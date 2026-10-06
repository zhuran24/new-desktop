//! 冻结对话经中间条目转换；不读取文件、不启动后端。
#![doc = include_str!("../README.md")]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
mod codec;
mod codex;
pub use codec::{decode, encode};
mod images;
pub use images::{FrozenImage, MAX_IMAGE_BYTES};
mod profile;
pub use profile::{
    Effort, MappedProfile, NativeProfile, Permission, Profile, ProfileTarget, decode_profile,
    encode_profile,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    Claude,
    Codex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeItem {
    /// 导出方提供的稳定原生位置，完整导出内不能重复。
    pub position: String,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenInput {
    pub backend: BackendKind,
    pub source_id: String,
    pub epoch: String,
    pub complete: bool,
    #[serde(default)]
    pub images: BTreeMap<String, FrozenImage>,
    pub items: Vec<NativeItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Part {
    Text(String),
    Notice {
        reason: String,
        value: Value,
    },
    Image {
        media_type: String,
        data: String,
    },
    Reasoning {
        backend: BackendKind,
        value: Value,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        content: Value,
        is_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub source_id: String,
    pub position: String,
    pub role: Role,
    pub parts: Vec<Part>,
    /// 源端特有字段随中间条目保存，不把它们伪装成另一家的原生内容。
    pub native: NativeItem,
    pub backend: BackendKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LossReport {
    pub entries: Vec<Loss>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loss {
    pub position: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decoded {
    pub entries: Vec<Entry>,
    pub loss: LossReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Encoded {
    pub items: Vec<Value>,
    pub loss: LossReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPoint {
    /// 格式和映射发生不兼容变化时提高修订号，旧点必须重建。
    pub codec_revision: u32,
    pub source_id: String,
    pub backend: BackendKind,
    pub target: BackendKind,
    pub epoch: String,
    pub count: usize,
    pub prefix_sha256: String,
    pub last_position: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Converted {
    pub items: Vec<Value>,
    pub sync: SyncPoint,
    pub loss: LossReport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvertError {
    /// 导出方未确认已取得完整有效历史。
    Incomplete,
    /// 损坏内容、附件散列、工具配对或位置身份。
    Invalid(String),
    /// 不得硬接旧目标；在同一段新建目标后整段转换。
    SyncInvalid,
}
impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ConvertError {}

pub fn convert(
    input: &FrozenInput,
    to: BackendKind,
    since: Option<&SyncPoint>,
) -> Result<Converted, ConvertError> {
    let start = if let Some(s) = since {
        if s.codec_revision != 1
            || s.source_id != input.source_id
            || s.backend != input.backend
            || s.target != to
            || s.epoch != input.epoch
            || s.count > input.items.len()
            || s.last_position.as_deref()
                != input.items[..s.count].last().map(|i| i.position.as_str())
            || s.prefix_sha256 != images::prefix_hash(input, s.count)
        {
            return Err(ConvertError::SyncInvalid);
        }
        s.count
    } else {
        0
    };
    let mut decoded = decode(input)?;
    decoded.entries.drain(..start);
    let encoded = encode(&decoded, to)?;
    Ok(Converted {
        items: encoded.items,
        sync: SyncPoint {
            codec_revision: 1,
            source_id: input.source_id.clone(),
            backend: input.backend,
            target: to,
            epoch: input.epoch.clone(),
            count: input.items.len(),
            prefix_sha256: images::prefix_hash(input, input.items.len()),
            last_position: input.items.last().map(|i| i.position.clone()),
        },
        loss: encoded.loss,
    })
}

fn hash(value: &(impl Serialize + ?Sized)) -> String {
    // Cargo feature unification may enable serde_json/preserve_order elsewhere.
    // Persisted sync points and replay IDs must not depend on map insertion order.
    let mut canonical = serde_json::to_value(value).expect("serializable history");
    canonical.sort_all_objects();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical).expect("JSON value"))
    )
}

fn validate_tools(entries: &[Entry]) -> Result<(), ConvertError> {
    let mut calls = BTreeSet::new();
    let mut pending = BTreeSet::new();
    for entry in entries {
        for part in &entry.parts {
            match part {
                Part::ToolCall { id, name, input } => {
                    if id.is_empty() || name.is_empty() || !input.is_object() || !calls.insert(id) {
                        return Err(ConvertError::Invalid(format!(
                            "invalid or duplicate tool call at {}",
                            entry.position
                        )));
                    }
                    pending.insert(id);
                }
                Part::ToolResult { id, .. } if !pending.remove(id) => {
                    return Err(ConvertError::Invalid(format!(
                        "unpaired tool result at {}",
                        entry.position
                    )));
                }
                _ => {}
            }
        }
    }
    if !pending.is_empty() {
        return Err(ConvertError::Invalid(
            "unfinished tool calls in export".into(),
        ));
    }
    Ok(())
}

fn field(value: &Value, key: &str) -> Result<String, ConvertError> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ConvertError::Invalid(format!("invalid {key}")))
}
