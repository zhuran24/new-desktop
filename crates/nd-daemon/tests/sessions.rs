//! 主接缝：同步副本（和 ndctl）对真守护进程讲 nd-wire；真 CLI、两个 mod、看守、systemd、SQLite，
//! 只换模型端点（离线伪端点）、缩短时间。验收：ndctl 新建会话并流式对话；回显带原 uuid 才算落地；
//! 创建失败的两种结果；闲置回收与按需拉起；有后台任务不回收；守护进程重启后接着用同一个后端进程。
#![cfg(feature = "scenarios")]
use nd_testkit::{ModelReply, Route, Scenario, ScenarioOptions};
use nd_ui_core::SyncReplica;
use nd_wire::{Command, CommandReply, Item, Receipt, Snapshot};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const MODEL: &str = "claude-haiku-4-5";

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "types" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Fixture {
    scenario: Scenario,
}

impl Fixture {
    /// 守护进程配 Claude 后端：钉住的 CLI（看守沙盒里叫 /cli）、仓库里的两个 mod、
    /// 场景自己的 CLAUDE_CONFIG_DIR；后端环境从白名单构造，只有离线端点和假 key。
    async fn start(name: &str, idle_reclaim_ms: u64) -> Self {
        let config = format!(
            r#"
[claude]
cli = "/cli"
hook_mod = "{{root}}/mods/new-desktop"
action_mod = "{{root}}/mods/new-desktop-actions"
config_dir = "{{root}}/claude"
inherit_env = false
record = true
hello_timeout_ms = 10000
poll_timeout_ms = 5000
init_timeout_ms = 30000

[claude.env]
PATH = "/usr/bin:/bin"
HOME = "/sandbox/home"
CLAUDE_CONFIG_DIR = "/sandbox/claude"
XDG_CONFIG_HOME = "/sandbox/config"
XDG_DATA_HOME = "/sandbox/data"
XDG_STATE_HOME = "/sandbox/state"
XDG_CACHE_HOME = "/sandbox/cache"
XDG_RUNTIME_DIR = "/sandbox/runtime"
LANG = "C.UTF-8"
TERM = "dumb"
ANTHROPIC_BASE_URL = "http://127.0.0.1:8765"
ANTHROPIC_API_KEY = "offline-fixture"
DISABLE_TELEMETRY = "1"
DISABLE_ERROR_REPORTING = "1"

[sessions]
idle_reclaim_ms = {idle_reclaim_ms}
tick_ms = 50
"#
        );
        let mut options = ScenarioOptions::new(
            name,
            std::env::var_os("ND_TEST_DAEMON").expect("run scripts/test-scenarios.sh"),
        );
        options.watchdog = Some(std::env::var_os("ND_TEST_WATCHDOG").unwrap().into());
        options.config = config;
        options.timeout = Duration::from_secs(20);
        let scenario = Scenario::start(options).await.unwrap();
        let mods = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods");
        for module in ["new-desktop", "new-desktop-actions"] {
            copy_dir(
                &mods.join(module),
                &scenario.root().join("mods").join(module),
            );
        }
        Self { scenario }
    }
    fn socket(&self) -> std::path::PathBuf {
        self.scenario.root().join("runtime/nd.sock")
    }
    fn main(&self) -> Route {
        Route::new(None, MODEL)
    }
    async fn ui(&self) -> SyncReplica {
        self.scenario.connect().await.unwrap()
    }
    /// 只看一眼会话流：订阅、取快照、断开。断开之后不算「有人在看」。
    async fn peek(&self, session: &str) -> Snapshot {
        let mut ui = self.ui().await;
        let snapshot = ui.subscribe(&format!("session/{session}")).await.unwrap();
        let _ = ui.close().await;
        snapshot
    }
    async fn wait(&self, session: &str, what: &str, until: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            let snapshot = self.peek(session).await;
            if until(&snapshot) {
                return snapshot;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}: {:#?}",
                snapshot.items
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn command(&self, id: &str, name: &str, args: Value) -> CommandReply {
        self.ui()
            .await
            .command(&Command {
                id: id.into(),
                device: "test".into(),
                name: name.into(),
                args,
                expect: json!({}),
            })
            .await
            .unwrap()
    }
    async fn create(&self, id: &str, cwd: &str, text: &str) -> String {
        match self
            .command(
                id,
                "session.create",
                json!({"cwd":cwd,"text":text,"model":MODEL}),
            )
            .await
        {
            CommandReply::Receipt {
                receipt:
                    Receipt::Accepted {
                        stream: Some(stream),
                        ..
                    },
            } => stream.trim_start_matches("session/").to_owned(),
            other => panic!("create: {other:?}"),
        }
    }
    async fn send(&self, id: &str, session: &str, text: &str) {
        let reply = self
            .command(id, "session.send", json!({"session":session,"text":text}))
            .await;
        assert!(
            matches!(
                &reply,
                CommandReply::Receipt {
                    receipt: Receipt::Done { .. }
                }
            ),
            "{reply:?}"
        );
    }
    async fn ndctl(&self, args: &[&str]) -> std::process::Output {
        let socket = self.socket();
        let mut all = vec!["--socket", socket.to_str().unwrap()];
        all.extend_from_slice(args);
        tokio::process::Command::new(env!("CARGO_BIN_EXE_ndctl"))
            .env_clear()
            .args(all)
            .output()
            .await
            .unwrap()
    }
    /// CLI 自己写下的记录文件（在场景的 CLAUDE_CONFIG_DIR 里）。
    fn transcript(&self, backend_session: &str) -> String {
        let projects = self.scenario.root().join("claude/projects");
        for dir in std::fs::read_dir(&projects).unwrap() {
            let path = dir.unwrap().path().join(format!("{backend_session}.jsonl"));
            if let Ok(text) = std::fs::read_to_string(&path) {
                return text;
            }
        }
        panic!("no transcript for {backend_session} under {projects:?}");
    }
    fn close(self) {
        self.scenario.close().unwrap();
    }
}

fn header(snapshot: &Snapshot) -> &Value {
    &snapshot
        .items
        .iter()
        .find(|i| i.id == "header")
        .unwrap_or_else(|| panic!("no header: {:#?}", snapshot.items))
        .data
}
fn has_header(snapshot: &Snapshot, pred: impl Fn(&Value) -> bool) -> bool {
    snapshot
        .items
        .iter()
        .any(|i| i.id == "header" && pred(&i.data))
}
fn prompt<'a>(snapshot: &'a Snapshot, text: &str) -> Option<&'a Item> {
    snapshot
        .items
        .iter()
        .find(|i| i.kind == "prompt" && i.data["text"] == text)
}
fn texts(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .items
        .iter()
        .filter(|i| i.kind == "text" && i.data["complete"] == true)
        .map(|i| i.data["text"].as_str().unwrap().to_owned())
        .collect()
}
fn request_text(body: &Value) -> String {
    body["messages"].to_string()
}

#[tokio::test]
async fn ndctl_creates_a_session_and_streams_the_reply() {
    let fx = Fixture::start("nd13-ndctl", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    let answer = "你好，我是离线模型。今天想做点什么？";
    endpoint.enqueue(fx.main(), ModelReply::streaming_text(answer, 3, 40));
    let out = fx
        .ndctl(&[
            "new",
            "--cwd",
            "/sandbox/project",
            "--model",
            MODEL,
            "--follow",
            "--timeout",
            "60",
            "你好",
        ])
        .await;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let session = lines[0]["session"].as_str().unwrap().to_owned();
    assert_eq!(lines[0]["reply"]["receipt"]["status"], "accepted");
    // 流式：同一个文字条目先以不完整的增量出现、越来越长，最后整块替换成完整的回答。
    let block: Vec<&Value> = lines
        .iter()
        .filter_map(|l| l.get("item"))
        .filter(|i| i["kind"] == "text")
        .collect();
    let partial: Vec<&str> = block
        .iter()
        .filter(|i| i["data"]["complete"] == false)
        .map(|i| i["data"]["text"].as_str().unwrap())
        .collect();
    assert!(partial.len() >= 2, "expected streamed deltas: {block:#?}");
    assert!(
        partial
            .windows(2)
            .all(|w| w[1].starts_with(w[0]) && w[1].len() > w[0].len())
    );
    let complete: Vec<&Value> = block
        .iter()
        .filter(|i| i["data"]["complete"] == true)
        .copied()
        .collect();
    assert_eq!(complete.len(), 1, "{block:#?}");
    assert_eq!(complete[0]["data"]["text"], answer);
    assert_eq!(
        complete[0]["id"], block[0]["id"],
        "deltas and the final block share one item"
    );
    // 模型收到的是这条消息。
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 1);
    assert!(request_text(&requests[0].body).contains("你好"));
    // 落地的那条消息的原生 uuid 就是 CLI 记录里那一行的 uuid。
    let snapshot = fx.peek(&session).await;
    let first = prompt(&snapshot, "你好").unwrap();
    assert_eq!(first.data["state"], "landed");
    let native = first.data["native"].as_str().unwrap().to_owned();
    let bs = header(&snapshot)["process"]["backend_session"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(fx.transcript(&bs).contains(&native));
    assert_eq!(header(&snapshot)["status"], "active");
    // 同一个会话接着聊：第二轮的模型请求带着第一轮。
    endpoint.enqueue(fx.main(), ModelReply::text("第二轮的回答"));
    let out = fx
        .ndctl(&["send", &session, "--follow", "--timeout", "60", "再说一句"])
        .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 2);
    let second = request_text(&requests[1].body);
    assert!(second.contains("你好") && second.contains(answer) && second.contains("再说一句"));
    let snapshot = fx.peek(&session).await;
    assert_eq!(texts(&snapshot), [answer, "第二轮的回答"]);
    // 全局列表里有这个会话。
    let global = fx.ui().await.subscribe("global").await.unwrap();
    assert!(
        global
            .items
            .iter()
            .any(|i| i.id == format!("session/{session}") && i.data["status"] == "active")
    );
    fx.close();
}
