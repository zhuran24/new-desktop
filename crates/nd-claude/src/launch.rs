//! Claude 后端进程的启动模板（规格「进程」：Claude Code 后端进程）。
use nd_mod_proto::{ModName, PluginOptions};
use nd_watchdog_proto::{LaunchSpec, Limits};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

/// owner 现有的四个 mod：在 New Desktop 的后端进程里经 `--settings` 的 `enabledPlugins` 关掉，
/// 不改它们的文件。
pub const OLD_MODS: [&str; 4] = ["codex-direct", "sendnow", "cc-quota", "ultracode-toggle"];

/// 启动模板固定加的环境；不可由配置覆盖。
pub const FIXED_ENV: [(&str, &str); 6] = [
    ("CLAUDE_CODE_ENABLE_FUNCTION_HOOKS", "1"),
    ("CLAUDE_CODE_FORK_SUBAGENT", "1"),
    ("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "true"),
    ("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1"),
    // 关掉 CLI 自动更新（手动 `claude update` 也被拒）。
    ("DISABLE_UPDATES", "1"),
    // 不监视 mod 目录（CLI 叫插件目录）；显式写 0，不依赖 print 模式的默认值。
    ("CLAUDE_CODE_PLUGIN_DIR_WATCH", "0"),
];

/// 从继承环境里去掉的变量：预加载脚本不许进后端进程。
pub const STRIPPED_ENV: [&str; 1] = ["BUN_OPTIONS"];

#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    /// CLI 二进制，按后端进程看到的路径写。
    pub cli: PathBuf,
    /// 钩子 mod（new-desktop）目录。
    pub hook_mod: PathBuf,
    /// 动作 mod（new-desktop-actions）目录。
    pub action_mod: PathBuf,
    /// mod 通道的 unix socket：本进程在此监听，mod 也按这个路径连（约 100 字节以内）。
    pub socket: PathBuf,
    /// 后端进程的基础环境：生产取守护进程继承的环境，测试从空白构造。
    pub env: BTreeMap<String, String>,
    /// 看守流水的软硬上限与溢出目录。
    pub limits: Limits,
    /// 等两个 hello 的时限，超时算拉起成功但只能聊天。
    pub hello_timeout: Duration,
    /// mod 长轮询挂起的时限；须小于 mod 一侧单次 fetch 的 30 秒上限。
    pub poll_timeout: Duration,
    /// 等 initialize 回应的时限。
    pub init_timeout: Duration,
}
impl ClaudeConfig {
    pub fn new(
        cli: impl Into<PathBuf>,
        hook_mod: impl Into<PathBuf>,
        action_mod: impl Into<PathBuf>,
        socket: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cli: cli.into(),
            hook_mod: hook_mod.into(),
            action_mod: action_mod.into(),
            socket: socket.into(),
            env: BTreeMap::new(),
            limits: Limits::default(),
            hello_timeout: Duration::from_secs(10),
            poll_timeout: Duration::from_secs(25),
            init_timeout: Duration::from_secs(30),
        }
    }
}

/// 新建用预定的后端会话 id；续接沿用原 id。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    Fresh { session: String },
    Resume { session: String },
}
impl Start {
    pub fn session(&self) -> &str {
        match self {
            Start::Fresh { session } | Start::Resume { session } => session,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Open {
    pub start: Start,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
}

/// `--settings` 的内联 JSON：给两个 mod 的 `pluginConfigs`，加上关掉四个旧 mod 的 `enabledPlugins`。
pub fn settings(config: &ClaudeConfig, run: &str) -> serde_json::Value {
    let options = PluginOptions {
        sock: config.socket.to_string_lossy().into_owned(),
        run: run.to_owned(),
    };
    let plugin_configs: serde_json::Map<_, _> = ModName::ALL
        .iter()
        .map(|m| (m.as_str().to_owned(), json!({ "options": options })))
        .collect();
    let disabled: serde_json::Map<_, _> = OLD_MODS
        .iter()
        .map(|name| (format!("{name}@skills-dir"), json!(false)))
        .collect();
    json!({ "pluginConfigs": plugin_configs, "enabledPlugins": disabled })
}

/// 看守进程据此拉起 CLI。参数逐项进 argv，不拼 shell；不带 `--await-initialize`，
/// 设置来源用 CLI 默认（不传 `--setting-sources`）。
pub fn launch_spec(config: &ClaudeConfig, run: &str, open: &Open) -> LaunchSpec {
    let mut argv: Vec<String> = [
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--permission-prompt-tool",
        "stdio",
        "--replay-user-messages",
        "--include-partial-messages",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for dir in [&config.hook_mod, &config.action_mod] {
        argv.push("--plugin-dir".into());
        argv.push(dir.to_string_lossy().into_owned());
    }
    argv.push("--settings".into());
    argv.push(settings(config, run).to_string());
    match &open.start {
        Start::Fresh { session } => argv.extend(["--session-id".into(), session.clone()]),
        Start::Resume { session } => argv.extend(["--resume".into(), session.clone()]),
    }
    if let Some(model) = &open.model {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(mode) = &open.permission_mode {
        argv.extend(["--permission-mode".into(), mode.clone()]);
    }
    let mut env = config.env.clone();
    for name in STRIPPED_ENV {
        env.remove(name);
    }
    for (key, value) in FIXED_ENV {
        env.insert(key.into(), value.into());
    }
    let mut limits = config.limits.clone();
    if !limits.overflow.as_os_str().is_empty() {
        limits.overflow = limits.overflow.join(run);
    }
    LaunchSpec {
        argv: std::iter::once(config.cli.to_string_lossy().into_owned())
            .chain(argv)
            .collect(),
        env,
        cwd: open.cwd.clone(),
        limits,
    }
}
