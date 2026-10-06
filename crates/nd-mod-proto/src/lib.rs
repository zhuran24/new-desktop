//! mod 协议：Claude 适配与两个 mod（钩子 mod、动作 mod）之间消息类型的唯一来源。
//!
//! 传输：mod 经守护进程的 unix socket 发 HTTP/1.1。`POST /hello`、长轮询 `GET /next`、
//! `POST /report`、`POST /result/<op_id>`。只加不改：新字段一律可缺省，新动作、新报告种类
//! 只追加变体。TypeScript 由 `nd-mod-schema` 从这里导出的 JSON Schema 生成，进仓库。
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod ts;

/// 协议版本；hello 里带上，守护进程据此兼容旧 mod。
pub const PROTO_VERSION: u32 = 1;
/// 两个 mod 与本 crate 同一版本号，plugin.json 的 version 与它一致。
pub const MOD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 两个 mod 的名字（plugin.json 的 name，也是 pluginConfigs 的键）。
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum ModName {
    #[serde(rename = "new-desktop")]
    Hook,
    #[serde(rename = "new-desktop-actions")]
    Actions,
}
impl ModName {
    pub const ALL: [ModName; 2] = [ModName::Hook, ModName::Actions];
    pub fn as_str(self) -> &'static str {
        match self {
            ModName::Hook => "new-desktop",
            ModName::Actions => "new-desktop-actions",
        }
    }
}

/// `--settings` 的 `pluginConfigs.<mod>.options`；字段在 plugin.json 的 userConfig 里声明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PluginOptions {
    /// 守护进程 mod 通道的 unix socket 路径（约 100 字节以内）。
    pub sock: String,
    /// 后端进程编号。
    pub run: String,
}

/// 发 hello 的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HelloCause {
    /// 模块装载（含热重载、worker 重生）后的 `session.start`。
    Start,
    /// `/clear` 之后 CLI 换了后端会话 id。
    Clear,
    /// 守护进程要求重报（例如守护进程重启后不认得这个绑定）。
    Rehello,
    /// 轮询前重读后端会话 id，发现变了。
    IdChanged,
}

/// `POST /hello`：mod 报到并绑定到（后端进程，后端会话）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hello {
    pub proto: u32,
    pub run: String,
    #[serde(rename = "mod")]
    pub module: ModName,
    pub mod_version: String,
    /// 每次模块装载新生成；重载后旧代次的结果查不到。
    pub mod_gen: String,
    /// `$.session.id()`：CLI 当前的后端会话 id。
    pub backend_session_id: String,
    /// `$.session.version().version`。
    pub cli_version: String,
    pub cause: HelloCause,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloReply {
    /// 绑定代次：这个后端进程每换一次后端会话 id 加一。
    pub binding_epoch: u64,
}

/// `GET /next` 的查询参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NextQuery {
    pub run: String,
    #[serde(rename = "mod")]
    pub module: ModName,
    pub mod_gen: String,
    pub backend_session_id: String,
}

/// `GET /next` 的回应：长轮询到时限、有命令或要求重报 hello 时返回。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Next {
    pub commands: Vec<Command>,
    /// 守护进程不认得这个绑定（或后端会话 id 刚换），mod 应重读 id 并重报 hello。
    pub rehello: bool,
}

/// 守护进程发给 mod 的一条命令。mod 执行前核对身份，对不上就拒绝。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Command {
    pub op_id: String,
    pub expected_backend_session_id: String,
    pub expected_mod_gen: String,
    pub action: Action,
}

/// 命令的动作。可重发类别见 [`Action::resend`]。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// 核对通道与身份，结果是 [`Pong`]。
    Ping,
    /// 按操作 id 查本代次留下的状态和结果，结果是 [`QueryAnswer`]。
    Query { op_ids: Vec<String> },
}

/// 重连后查不到结果时能不能用同一个操作 id 再发。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Resend {
    Resendable,
    /// 查不到结果就记 Unknown，不再发。
    NotResendable,
}
impl Action {
    /// 规格「两个 mod」：退役、解除退役、导出、列设置行、ping 可重发；派子代理、调工具、
    /// 压缩、给子代理发消息不可重发。只读的 query 可重发。
    pub fn resend(&self) -> Resend {
        match self {
            Action::Ping | Action::Query { .. } => Resend::Resendable,
        }
    }
}

/// `POST /result/<op_id>` 的正文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResultPost {
    pub run: String,
    #[serde(rename = "mod")]
    pub module: ModName,
    pub mod_gen: String,
    pub backend_session_id: String,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Done {
        value: Value,
    },
    Failed {
        error: String,
    },
    /// mod 没有执行这条命令。
    Rejected {
        reason: Rejection,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Rejection {
    /// 命令带的后端会话 id 不是当前的（例如 `/clear` 之后）。
    StaleSession { current: String },
    /// 命令带的 mod 代次不是当前装载的。
    StaleGen { current: String },
    /// 这个 mod 不认识或不执行这种动作。
    Unsupported,
}

/// ping 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Pong {
    pub backend_session_id: String,
    pub mod_gen: String,
}

/// query 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueryAnswer {
    pub ops: Vec<OpState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpState {
    pub op_id: String,
    pub phase: OpPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpPhase {
    /// 本代次没见过这个操作 id。
    Unknown,
    Running,
    Done,
}

/// `POST /report`：mod 主动上报的事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub run: String,
    #[serde(rename = "mod")]
    pub module: ModName,
    pub mod_gen: String,
    pub backend_session_id: String,
    /// 由 mod 生成，重发时不变，守护进程据此去重。
    pub report_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<String>,
    pub body: ReportBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportBody {
    /// `session.end`：`reason` 原样转发（`clear`、`other` 等）。
    SessionEnd {
        reason: String,
        ended_session_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportAck {
    /// 只有报告已落库才为 true；false 表示守护进程只收在内存里。
    pub durable: bool,
}

/// 非 200 回应的正文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorReply {
    pub code: String,
    pub message: String,
}

/// 把全部消息类型挂在一个根上，供 JSON Schema 与 TypeScript 一次导出。
#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct ModProtocol {
    options: PluginOptions,
    hello: Hello,
    hello_reply: HelloReply,
    next_query: NextQuery,
    next: Next,
    result: ResultPost,
    pong: Pong,
    query_answer: QueryAnswer,
    report: Report,
    report_ack: ReportAck,
    error: ErrorReply,
}

/// 公开的 JSON Schema（`protocol/mod.schema.json`）。
pub fn schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ModProtocol)).expect("schema serializes")
}

/// 两个 mod 的 `hooks/proto.ts`：类型与运行时常量，不碰 `$`、不用 `import()`。
pub fn typescript_module() -> String {
    format!(
        "// 由 nd-mod-schema 从 crates/nd-mod-proto 生成，不要手改。\n\
         // 重新生成：cargo run -p nd-mod-proto --bin nd-mod-schema -- .\n\n\
         export const PROTO_VERSION = {PROTO_VERSION};\n\
         export const MOD_VERSION = {MOD_VERSION:?};\n\
         export const HOOK_MOD: ModName = {:?};\n\
         export const ACTION_MOD: ModName = {:?};\n\n{}",
        ModName::Hook.as_str(),
        ModName::Actions.as_str(),
        ts::typescript(&schema())
    )
}
