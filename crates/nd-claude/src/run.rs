//! 拉起 Claude 后端进程与就绪判定（规格「进程」：先两个 hello 再 initialize）。
use crate::{
    Result,
    channel::{Binding, CommandResult, ModChannel, Recorded},
    launch::{ClaudeConfig, Open, launch_spec},
};
use nd_mod_proto::{Action, Command, Hello, ModName};
use nd_runs::{WatchLink, Watchdogs};
use nd_watchdog_proto::{Event, Identity, Record};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// initialize 的可选声明。`perTaskStopAffordance`、`forwardSubagentText` 规格要求总是声明。
#[derive(Clone, Debug)]
pub struct InitOptions {
    /// `supportedDialogKinds`：只列界面会画的对话框种类。
    pub dialog_kinds: Vec<String>,
    /// 要钩子 mod 路由才可用的代理定义（例如 Codex 子代理）。只能聊天时不声明，
    /// 免得约定的模型名被发给 Anthropic。
    pub hook_agents: serde_json::Map<String, Value>,
    /// 产品总是 true；只给 R10-E1 的对照实验关掉。
    pub per_task_stop_affordance: bool,
}
impl Default for InitOptions {
    fn default() -> Self {
        Self {
            dialog_kinds: vec![],
            hook_agents: serde_json::Map::new(),
            per_task_stop_affordance: true,
        }
    }
}

/// 进程能做什么。`ChatOnly`：hello 没等到（或会话 id 对不上），拉起仍算成功，但只能聊天。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "readiness", rename_all = "snake_case")]
pub enum Readiness {
    Full,
    ChatOnly { why: String },
}

/// 只能聊天时不可用的功能（规格「进程」：hello 超时）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Retire,
    CodexSubagent,
    TellSubagent,
    Summarize,
    BangMode,
    ForkSubagent,
    SettingsRows,
    /// 当前模型用原生用法操作转接来的任务；不可用时转接清单只能看结果。
    TaskOps,
}
impl Feature {
    pub const ALL: [Feature; 8] = [
        Feature::Retire,
        Feature::CodexSubagent,
        Feature::TellSubagent,
        Feature::Summarize,
        Feature::BangMode,
        Feature::ForkSubagent,
        Feature::SettingsRows,
        Feature::TaskOps,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unsupported { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    pub readiness: Readiness,
    pub features: BTreeMap<Feature, Availability>,
    /// Esc（`interrupt`）不停后台的子代理与 Workflow：声明了 perTaskStopAffordance 且 stdin 开着。
    pub interrupt_spares_background: bool,
}

/// 就绪：initialize 已回应、两个 hello 都报了预期的后端会话 id（只能聊天时只有回应）。
#[derive(Clone, Debug)]
pub struct Ready {
    pub run: String,
    pub backend_session_id: String,
    pub identity: Identity,
    pub hellos: BTreeMap<ModName, Hello>,
    pub caps: Caps,
    /// initialize 回应的 `response.response`，原样保留（开放解码）。
    pub initialize: Value,
}

pub struct Claude {
    config: ClaudeConfig,
    watchdogs: Arc<Watchdogs>,
    channel: ModChannel,
}

impl Claude {
    /// 在配置的 socket 上开 mod 通道。
    pub fn new(config: ClaudeConfig, watchdogs: Arc<Watchdogs>) -> Result<Self> {
        let channel = ModChannel::bind(&config.socket, config.poll_timeout)?;
        Ok(Self {
            config,
            watchdogs,
            channel,
        })
    }
    pub fn config(&self) -> &ClaudeConfig {
        &self.config
    }
    pub fn channel(&self) -> &ModChannel {
        &self.channel
    }

    /// 拉起后端进程并等它就绪：两个 hello（报预期的后端会话 id）→ 写 initialize → 等回应。
    /// hello 超时不算失败：发不带钩子代理的 initialize，结果是 `ChatOnly`。
    pub async fn open(&self, run: &str, open: Open, init: InitOptions) -> Result<ClaudeRun> {
        let session = open.start.session().to_owned();
        self.channel.register(run, &session);
        let spec = launch_spec(&self.config, run, &open);
        let launched = self.watchdogs.launch(run, spec).await?;
        let mut link = self.watchdogs.link(run).await?;
        let binding = self
            .channel
            .wait_binding(run, self.config.hello_timeout, |b| b.mods.len() == 2)
            .await
            .ok_or("run unregistered while waiting for hellos")?;
        let missing: Vec<&str> = ModName::ALL
            .iter()
            .filter(|m| !binding.mods.contains_key(m))
            .map(|m| m.as_str())
            .collect();
        let readiness = if missing.is_empty() {
            Readiness::Full
        } else {
            Readiness::ChatOnly {
                why: format!(
                    "{} 没有在 {} 毫秒内报到后端会话 {session}",
                    missing.join("、"),
                    self.config.hello_timeout.as_millis()
                ),
            }
        };
        let id = format!("nd-init-{run}");
        let mut request = json!({
            "subtype": "initialize",
            "perTaskStopAffordance": init.per_task_stop_affordance,
            "forwardSubagentText": true,
            "supportedDialogKinds": init.dialog_kinds,
        });
        if readiness == Readiness::Full && !init.hook_agents.is_empty() {
            request["agents"] = Value::Object(init.hook_agents.clone());
        }
        let frame = json!({"type":"control_request","request_id":id,"request":request});
        link.write(1, &frame.to_string()).await?;
        let (initialize, cursor) = await_response(&mut link, &id, self.config.init_timeout).await?;
        self.channel.settle(run);
        let features = Feature::ALL
            .iter()
            .map(|f| {
                (
                    *f,
                    match &readiness {
                        Readiness::Full => Availability::Available,
                        Readiness::ChatOnly { why } => {
                            Availability::Unsupported { why: why.clone() }
                        }
                    },
                )
            })
            .collect();
        Ok(ClaudeRun {
            ready: Ready {
                run: run.to_owned(),
                backend_session_id: session,
                identity: launched.identity,
                hellos: binding.mods,
                caps: Caps {
                    readiness,
                    features,
                    interrupt_spares_background: init.per_task_stop_affordance,
                },
                initialize,
            },
            link,
            next_in: 2,
            cursor,
            channel: self.channel.clone(),
        })
    }
}

/// 读看守流水直到 initialize 的回应；进程先退出或超时都是拉起失败。
async fn await_response(link: &mut WatchLink, id: &str, timeout: Duration) -> Result<(Value, u64)> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut cursor = 0;
    loop {
        for record in link.read(cursor, 1000).await? {
            cursor = record.end_seq;
            match &record.event {
                Event::Out { line } => {
                    let Ok(frame) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    if frame["type"] == "control_response" && frame["response"]["request_id"] == id
                    {
                        if frame["response"]["subtype"] == "success" {
                            return Ok((frame["response"]["response"].clone(), cursor));
                        }
                        return Err(format!("initialize failed: {}", frame["response"]).into());
                    }
                }
                Event::Exit { code } => {
                    return Err(
                        format!("backend exited with {code} before initialize answered").into(),
                    );
                }
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("initialize response timed out".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 已就绪的后端进程：持有看守连接（唯一的 stdin 写入者）和它的 mod 通道。
pub struct ClaudeRun {
    ready: Ready,
    link: WatchLink,
    next_in: u64,
    cursor: u64,
    channel: ModChannel,
}

impl ClaudeRun {
    pub fn ready(&self) -> &Ready {
        &self.ready
    }
    pub fn run(&self) -> &str {
        &self.ready.run
    }
    /// 写一行 stdin，返回看守分配的输入序号。
    pub async fn write(&mut self, frame: &Value) -> Result<u64> {
        let seq = self.next_in;
        self.link.write(seq, &frame.to_string()).await?;
        self.next_in += 1;
        Ok(seq)
    }
    /// 读 initialize 回应之后的看守流水，游标随之前进。
    pub async fn read(&mut self, limit: usize) -> Result<Vec<Record>> {
        let records = self.link.read(self.cursor, limit).await?;
        if let Some(last) = records.last() {
            self.cursor = last.end_seq;
        }
        Ok(records)
    }
    /// 读 stdout 直到某帧满足条件；之前的帧一并返回。
    pub async fn wait_frame(
        &mut self,
        timeout: Duration,
        mut matches: impl FnMut(&Value) -> bool,
    ) -> Result<Vec<Value>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut seen = vec![];
        loop {
            for record in self.read(1000).await? {
                if let Event::Out { line } = record.event
                    && let Ok(frame) = serde_json::from_str::<Value>(&line)
                {
                    let hit = matches(&frame);
                    seen.push(frame);
                    if hit {
                        return Ok(seen);
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("no matching frame; last: {:?}", seen.last()).into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub fn binding(&self) -> Binding {
        self.channel
            .binding(&self.ready.run)
            .expect("registered while the run is open")
    }
    pub async fn wait_binding(
        &self,
        timeout: Duration,
        until: impl Fn(&Binding) -> bool,
    ) -> Option<Binding> {
        self.channel
            .wait_binding(&self.ready.run, timeout, until)
            .await
    }
    /// 按当前绑定发命令并等结论；None 表示到时限仍无结论（交付不明）。
    pub async fn command(
        &self,
        module: ModName,
        action: Action,
        timeout: Duration,
    ) -> Option<CommandResult> {
        let binding = self.binding();
        let mod_gen = binding.mods.get(&module)?.mod_gen.clone();
        self.command_as(
            module,
            action,
            &binding.backend_session_id,
            &mod_gen,
            timeout,
        )
        .await
    }
    /// 带指定期望身份发命令（核对过时 id 的拒绝）。
    pub async fn command_as(
        &self,
        module: ModName,
        action: Action,
        expected_backend_session_id: &str,
        expected_mod_gen: &str,
        timeout: Duration,
    ) -> Option<CommandResult> {
        let op_id = uuid::Uuid::new_v4().to_string();
        self.channel.send(
            &self.ready.run,
            module,
            Command {
                op_id: op_id.clone(),
                expected_backend_session_id: expected_backend_session_id.to_owned(),
                expected_mod_gen: expected_mod_gen.to_owned(),
                action,
            },
        );
        self.channel.result(&self.ready.run, &op_id, timeout).await
    }
    pub fn recording(&self) -> Vec<Recorded> {
        self.channel.recording(&self.ready.run)
    }
}
