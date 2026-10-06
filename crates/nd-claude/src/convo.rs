//! 对话的协议状态机：把看守流水的每条记录（CLI 的 stdout 行、我方写的 stdin 行、退出、缺口）
//! 归一成对话事实。纯计算，不做 I/O；状态可序列化，随检查点提交，守护进程重启后接着用。
//!
//! 同一串记录录下来，回放时喂给新的状态机，得到同样的事实（录制回归）。
//!
//! 要点（2.1.289，`research/impl/cli-protocol.md` §4、§6）：
//! - 送达只认带原 uuid 的 `user{isReplay:true}` 回显；写进管道、`queued`、`result` 都不算。
//! - `result` 是回合结束，不是模型回复，也可能是 `shouldQuery:false` 的空回合。
//! - 内容块身份是（API 消息 id，块序号）；`stream_event` 的增量之后到的完整块整体替换。
//! - 子代理的帧（`parent_tool_use_id` 非空）不进主对话。
//! - 后台任务按 `task_started`、`task_notification`、`task_updated`、`background_tasks_changed`
//!   记；用过定时事项或流水丢过行就说不清，`Drain::Unknown`，不当成空。
use nd_backend::{Drain, Item, ItemKind};
use nd_watchdog_proto::{Event, GapReason, Record};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// 归一化的对话事实，回放比对的单位。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "convo", rename_all = "snake_case")]
pub enum Convo {
    /// 我方写进 stdin 的 user 行（看守流水的输入记录）：已写出。
    Written {
        uuid: String,
    },
    /// 带原 uuid 的回显：送达。
    Echo {
        uuid: String,
    },
    TurnStarted,
    /// 实际回合的提示集合；同一回合可包含多条 user，工具调用不另开轮。
    TurnMapped {
        turn: String,
        uuids: Vec<String>,
        complete: bool,
        last_assistant: Option<String>,
    },
    /// 回合结束；`uuids` 是这一回合消费的用户消息。
    TurnEnded {
        ok: bool,
        subtype: String,
        error: Option<String>,
        uuids: Vec<String>,
    },
    Delta {
        item: String,
        kind: ItemKind,
        text: String,
    },
    Block {
        item: Item,
    },
    /// 排队命令的去向（queued、started、completed、cancelled、discarded、refused）。
    Lifecycle {
        uuid: String,
        state: String,
    },
    /// CLI 对我方控制请求的回应。
    Reply {
        request_id: String,
        ok: bool,
        body: Value,
    },
    /// CLI 发来、等宿主回答的请求（审批、提问……）。
    Asked {
        request_id: String,
        subtype: String,
        raw: Value,
    },
    Tasks {
        drain: Drain,
    },
    Exit {
        code: i32,
    },
    Gap {
        lost: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// 每条 API 消息已完成的块数：下一个完整块的序号。
    blocks: BTreeMap<String, usize>,
    /// 主对话当前流式输出的消息 id（`message_start` 给的）。
    streaming: Option<String>,
    tasks: BTreeSet<String>,
    schedules: bool,
    lost: bool,
    reported: Option<Drain>,
    running: bool,
    #[serde(default)]
    turn_key: Option<String>,
    #[serde(default)]
    turn_uuids: Vec<String>,
    #[serde(default)]
    last_assistant: Option<String>,
}

const SCHEDULE_TOOLS: [&str; 2] = ["CronCreate", "ScheduleWakeup"];

fn block_text(block: &Value) -> (ItemKind, String) {
    match block["type"].as_str() {
        Some("text") => (
            ItemKind::Text,
            block["text"].as_str().unwrap_or_default().into(),
        ),
        Some("thinking") => (
            ItemKind::Thinking,
            block["thinking"].as_str().unwrap_or_default().into(),
        ),
        Some("tool_use") => (
            ItemKind::ToolUse,
            format!(
                "{} {}",
                block["name"].as_str().unwrap_or("tool"),
                block["input"]
            ),
        ),
        Some("tool_result") => (
            ItemKind::ToolResult,
            match &block["content"] {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            },
        ),
        _ => (ItemKind::Other, block.to_string()),
    }
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前的收尾判据。
    pub fn drain(&self) -> Drain {
        if self.lost {
            Drain::Unknown {
                why: "看守流水丢过重建不出的行".into(),
            }
        } else if self.schedules {
            Drain::Unknown {
                why: "用过定时事项，CLI 不在 stdout 报它们的清单".into(),
            }
        } else if self.tasks.is_empty() {
            Drain::Drained
        } else {
            Drain::Busy
        }
    }

    fn drain_changed(&mut self, out: &mut Vec<Convo>) {
        let now = self.drain();
        if self.reported.as_ref() != Some(&now) {
            self.reported = Some(now.clone());
            out.push(Convo::Tasks { drain: now });
        }
    }

    pub fn apply(&mut self, record: &Record) -> Vec<Convo> {
        let mut out = vec![];
        match &record.event {
            Event::In { line, .. } => {
                if let Ok(frame) = serde_json::from_str::<Value>(line)
                    && frame["type"] == "user"
                    && let Some(uuid) = frame["uuid"].as_str()
                {
                    out.push(Convo::Written { uuid: uuid.into() });
                }
            }
            Event::Out { line } => {
                if let Ok(frame) = serde_json::from_str::<Value>(line) {
                    self.frame(&frame, &mut out);
                }
            }
            Event::Exit { code } => out.push(Convo::Exit { code: *code }),
            Event::Gap { reason } => {
                let lost = matches!(reason, GapReason::LostLines);
                out.push(Convo::Gap { lost });
                if lost {
                    self.lost = true;
                    self.drain_changed(&mut out);
                }
            }
            Event::Err { .. } | Event::StderrTruncated => {}
        }
        out
    }

    fn map_turn(&mut self, frame: &Value, out: &mut Vec<Convo>) {
        if frame["type"] == "system" && frame["subtype"] == "init" {
            self.turn_key = frame["uuid"].as_str().map(str::to_owned);
            self.turn_uuids.clear();
            self.last_assistant = None;
            return;
        }
        let complete = frame["type"] == "result";
        if !(complete
            || frame["type"] == "assistant"
            || (frame["type"] == "stream_event" && frame["event"]["type"] == "message_start"))
        {
            return;
        }
        let mut uuids: Vec<String> = frame["user_message_uuids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|u| u.as_str().map(str::to_owned))
            .collect();
        if uuids.is_empty()
            && let Some(id) = frame["user_message_uuid"].as_str()
        {
            uuids.push(id.into());
        }
        for id in uuids {
            if !self.turn_uuids.contains(&id) {
                self.turn_uuids.push(id);
            }
        }
        if frame["type"] == "assistant" {
            self.last_assistant = frame["uuid"].as_str().map(str::to_owned);
        }
        // 无法识别回合归属时不按文本、票序或发送次数猜轮。
        if self.turn_key.is_none() {
            self.turn_key = self.turn_uuids.first().cloned();
        }
        if let Some(turn) = &self.turn_key
            && !self.turn_uuids.is_empty()
        {
            out.push(Convo::TurnMapped {
                turn: turn.clone(),
                uuids: self.turn_uuids.clone(),
                complete,
                last_assistant: self.last_assistant.clone(),
            });
        }
        if complete {
            self.turn_key = None;
            self.turn_uuids.clear();
            self.last_assistant = None;
        }
    }

    fn frame(&mut self, frame: &Value, out: &mut Vec<Convo>) {
        let main = frame["parent_tool_use_id"].is_null();
        if main {
            self.map_turn(frame, out);
        }
        match frame["type"].as_str() {
            Some("user") => {
                if frame["isReplay"] == true {
                    if let Some(uuid) = frame["uuid"].as_str() {
                        out.push(Convo::Echo { uuid: uuid.into() });
                    }
                } else if main && let Some(blocks) = frame["message"]["content"].as_array() {
                    for block in blocks.iter().filter(|b| b["type"] == "tool_result") {
                        let (kind, text) = block_text(block);
                        out.push(Convo::Block {
                            item: Item {
                                id: format!(
                                    "result:{}",
                                    block["tool_use_id"].as_str().unwrap_or_default()
                                ),
                                kind,
                                text,
                                raw: block.clone(),
                            },
                        });
                    }
                }
            }
            Some("assistant") => {
                let content = frame["message"]["content"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for block in &content {
                    if block["type"] == "tool_use"
                        && block["name"]
                            .as_str()
                            .is_some_and(|n| SCHEDULE_TOOLS.contains(&n))
                    {
                        self.schedules = true;
                    }
                }
                self.drain_changed(out);
                if !main {
                    return;
                }
                let message = frame["message"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                for block in content {
                    let next = self.blocks.entry(message.clone()).or_default();
                    let index = *next;
                    *next += 1;
                    let (kind, text) = block_text(&block);
                    out.push(Convo::Block {
                        item: Item {
                            id: format!("{message}:{index}"),
                            kind,
                            text,
                            raw: block,
                        },
                    });
                }
            }
            Some("stream_event") if main => {
                let event = &frame["event"];
                match event["type"].as_str() {
                    Some("message_start") => {
                        self.streaming = event["message"]["id"].as_str().map(str::to_owned);
                    }
                    Some("content_block_delta") => {
                        let Some(message) = &self.streaming else {
                            return;
                        };
                        let index = event["index"].as_u64().unwrap_or_default();
                        let delta = &event["delta"];
                        let (kind, text) = match delta["type"].as_str() {
                            Some("text_delta") => (ItemKind::Text, delta["text"].as_str()),
                            Some("thinking_delta") => {
                                (ItemKind::Thinking, delta["thinking"].as_str())
                            }
                            _ => return,
                        };
                        if let Some(text) = text {
                            out.push(Convo::Delta {
                                item: format!("{message}:{index}"),
                                kind,
                                text: text.into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("result") if main => {
                let subtype = frame["subtype"].as_str().unwrap_or("unknown").to_owned();
                let ok = subtype == "success" && frame["is_error"] != true;
                let error = if ok {
                    None
                } else {
                    Some(match frame["errors"].as_array() {
                        Some(errors) if !errors.is_empty() => errors
                            .iter()
                            .map(|e| e.as_str().map(str::to_owned).unwrap_or(e.to_string()))
                            .collect::<Vec<_>>()
                            .join("; "),
                        _ => frame["result"].as_str().unwrap_or(&subtype).to_owned(),
                    })
                };
                let mut uuids: Vec<String> = frame["user_message_uuids"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|u| u.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if uuids.is_empty()
                    && let Some(uuid) = frame["user_message_uuid"].as_str()
                {
                    uuids.push(uuid.into());
                }
                self.running = false;
                out.push(Convo::TurnEnded {
                    ok,
                    subtype,
                    error,
                    uuids,
                });
            }
            Some("system") => match frame["subtype"].as_str() {
                Some("init") if main => {
                    self.running = true;
                    out.push(Convo::TurnStarted);
                }
                Some("task_started") => {
                    if let Some(id) = frame["task_id"].as_str() {
                        self.tasks.insert(id.into());
                    }
                    self.drain_changed(out);
                }
                Some("task_notification") => {
                    if let Some(id) = frame["task_id"].as_str() {
                        self.tasks.remove(id);
                    }
                    self.drain_changed(out);
                }
                Some("task_updated") => {
                    if let Some(id) = frame["task_id"].as_str()
                        && frame["patch"]["status"].as_str().is_some_and(|s| {
                            matches!(s, "completed" | "failed" | "killed" | "stopped")
                        })
                    {
                        self.tasks.remove(id);
                    }
                    self.drain_changed(out);
                }
                Some("background_tasks_changed") => {
                    self.tasks = frame["tasks"]
                        .as_array()
                        .map(|tasks| {
                            tasks
                                .iter()
                                .filter_map(|t| t["task_id"].as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.drain_changed(out);
                }
                _ => {}
            },
            Some("command_lifecycle") => {
                if let (Some(uuid), Some(state)) =
                    (frame["command_uuid"].as_str(), frame["state"].as_str())
                {
                    out.push(Convo::Lifecycle {
                        uuid: uuid.into(),
                        state: state.into(),
                    });
                }
            }
            Some("control_response") => {
                let response = &frame["response"];
                if let Some(id) = response["request_id"].as_str() {
                    out.push(Convo::Reply {
                        request_id: id.into(),
                        ok: response["subtype"] == "success",
                        body: response.clone(),
                    });
                }
            }
            Some("control_request") => {
                if let Some(id) = frame["request_id"].as_str() {
                    out.push(Convo::Asked {
                        request_id: id.into(),
                        subtype: frame["request"]["subtype"]
                            .as_str()
                            .unwrap_or("unknown")
                            .into(),
                        raw: frame["request"].clone(),
                    });
                }
            }
            _ => {}
        }
    }
}

/// 回放：把一串记录喂给新的状态机，得到每条记录的事实。
pub fn replay(records: &[Record]) -> Vec<Vec<Convo>> {
    let mut conversation = Conversation::new();
    records.iter().map(|r| conversation.apply(r)).collect()
}
