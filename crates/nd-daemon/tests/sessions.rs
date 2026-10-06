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
async fn new_session_models_come_from_the_backend_before_any_conversation() {
    let fx = Fixture::start("nd14-models", 3_600_000).await;
    let mut ui = fx.ui().await;
    let models = ui.models("claude", "/sandbox/project").await.unwrap();
    assert!(
        !models.is_empty(),
        "initialize must advertise selectable models"
    );
    assert!(models.iter().any(|m| m.value == "haiku" && !m.disabled));
    assert!(models.iter().all(|m| !m.label.is_empty()));
    assert!(
        ui.get("sessions", Default::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        fx.scenario.endpoint().requests().is_empty(),
        "listing models must not prompt"
    );
    assert!(ui.models("claude", "/missing-directory").await.is_err());
    assert!(
        ui.models("not-installed", "/sandbox/project")
            .await
            .is_err()
    );
    fx.close();
}

#[tokio::test]
async fn desktop_client_cold_reopens_during_a_delta_and_continues_the_conversation() {
    use nd_ui_core::{CommandClient, FeedUpdate, ReplicaFeed};
    let fx = Fixture::start("nd14-cold", 3_600_000).await;
    let client = CommandClient::start(fx.socket()).unwrap();
    let models = client
        .models("claude".into(), "/sandbox/project".into())
        .await
        .unwrap();
    let model = models.iter().find(|m| m.value == "haiku").unwrap();
    let answer = "# 中文回答\n\n```rust\nfn main() { println!(\"你好\"); }\n```\n流式尾巴";
    fx.scenario.endpoint().enqueue(
        Route::new(None, "claude-haiku-4-5-20251001"),
        ModelReply::streaming_text(answer, 1, 90),
    );
    let reply = client
        .command(Command {
            id: "desktop-create".into(),
            device: "desktop".into(),
            name: "session.create".into(),
            args: json!({"cwd":"/sandbox/project", "model":model.value, "text":"请写代码"}),
            expect: json!({}),
        })
        .await
        .unwrap();
    let CommandReply::Receipt {
        receipt: Receipt::Accepted {
            stream: Some(stream),
            ..
        },
    } = reply
    else {
        panic!("{reply:?}")
    };
    let mut feed = ReplicaFeed::start(fx.socket(), &stream).unwrap();
    let partial = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(FeedUpdate::Snapshot(s)) = feed.recv().await
                && s.items.iter().any(|i| {
                    i.kind == "text"
                        && i.data["complete"] == false
                        && !i.data["text"].as_str().unwrap().is_empty()
                })
            {
                break s;
            }
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "{e}: requests={:?}",
            fx.scenario
                .endpoint()
                .requests()
                .iter()
                .map(|r| &r.route)
                .collect::<Vec<_>>()
        )
    });
    let block = partial.items.iter().find(|i| i.kind == "text").unwrap();
    let identity = header(&partial)["process"]["run"].clone();
    // 丢掉全部界面副本与命令连接；新实例只能靠冷快照恢复累积内容。
    drop(feed);
    drop(client);
    let mut reopened = ReplicaFeed::start(fx.socket(), &stream).unwrap();
    let Some(FeedUpdate::Snapshot(cold)) = reopened.recv().await else {
        panic!("cold snapshot missing")
    };
    let resumed = cold.items.iter().find(|i| i.id == block.id).unwrap();
    assert!(
        resumed.data["text"]
            .as_str()
            .unwrap()
            .starts_with(block.data["text"].as_str().unwrap())
    );
    assert_eq!(header(&cold)["process"]["run"], identity);
    let session = stream.trim_start_matches("session/");
    let finished = fx
        .wait(session, "complete Markdown", |s| texts(s) == [answer])
        .await;
    assert_eq!(
        finished.items.iter().filter(|i| i.id == block.id).count(),
        1
    );
    assert_eq!(fx.scenario.endpoint().requests().len(), 1);
    let client = CommandClient::start(fx.socket()).unwrap();
    fx.scenario.endpoint().enqueue(
        Route::new(None, "claude-haiku-4-5-20251001"),
        ModelReply::text("继续回答"),
    );
    let sent = client
        .command(Command {
            id: "desktop-send".into(),
            device: "desktop".into(),
            name: "session.send".into(),
            args: json!({"session":session,"text":"继续"}),
            expect: json!({}),
        })
        .await
        .unwrap();
    assert!(matches!(
        sent,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    fx.wait(session, "second turn", |s| {
        texts(s).contains(&"继续回答".to_owned())
    })
    .await;
    let requests = fx.scenario.endpoint().requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[1].body).contains("请写代码"));
    reopened.close().await;
    drop(client);
    fx.close();
}

#[tokio::test]
async fn native_chat_window_creates_and_recovers_during_streaming_markdown() {
    let fx = Fixture::start("nd14-window", 3_600_000).await;
    let answer = "# 中文回答\n\n一段 **Markdown**。\n\n```rust\nfn main() { println!(\"你好\"); }\n```\n\n结束。";
    fx.scenario.endpoint().enqueue(
        Route::new(None, "claude-haiku-4-5-20251001"),
        ModelReply::streaming_text(answer, 1, 100),
    );
    let desktop = std::env::var_os("ND_TEST_DESKTOP").expect("run scripts/test-scenarios.sh");
    let output = std::env::var_os("ND_NATIVE_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| fx.scenario.root().join("native-chat"));
    let result = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
        .arg("--desktop")
        .arg(desktop)
        .arg("--socket")
        .arg(fx.socket())
        .arg("--output")
        .arg(&output)
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let verdict: Value =
        serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap();
    assert_eq!(verdict["pass"], true);
    assert_eq!(
        fx.scenario.endpoint().requests().len(),
        1,
        "reopening the UI must not prompt again"
    );
    let session = verdict["session"].as_str().unwrap();
    let done = fx.peek(session).await;
    assert_eq!(texts(&done), [answer]);
    assert!(header(&done)["process"]["alive"].as_bool().unwrap());
    fx.close();
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

/// 当前后端进程的 pid：会话头报的后端进程编号，在看守托管那里查身份。
async fn cli_pid(fx: &Fixture, snapshot: &Snapshot) -> i32 {
    let run = header(snapshot)["process"]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = fx.ui().await.get("runs", Default::default()).await.unwrap();
    let found = page
        .items
        .iter()
        .find(|i| i.data["run"] == run)
        .unwrap_or_else(|| panic!("no run {run}: {:#?}", page.items));
    found.data["identity"]["pid"].as_i64().unwrap() as i32
}

fn signal(pid: i32, signal: rustix::process::Signal) {
    rustix::process::kill_process(rustix::process::Pid::from_raw(pid).unwrap(), signal).unwrap();
}

async fn listed(fx: &Fixture, session: &str) -> Option<Item> {
    let page = fx
        .ui()
        .await
        .get("sessions", Default::default())
        .await
        .unwrap();
    page.items
        .into_iter()
        .find(|i| i.id == format!("session/{session}"))
}

#[tokio::test]
async fn a_message_counts_as_landed_only_when_the_cli_echoes_its_uuid() {
    let fx = Fixture::start("nd13-echo", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("第一轮"));
    let session = fx.create("echo-create", "/sandbox/project", "你好").await;
    let snapshot = fx
        .wait(&session, "first turn done", |s| {
            has_header(s, |h| {
                h["status"] == "active" && h["process"]["turn_running"] == false
            }) && texts(s) == ["第一轮"]
        })
        .await;
    let pid = cli_pid(&fx, &snapshot).await;
    // 后端进程停住：这条消息写进了看守（已写出），CLI 还没读到，不会有回显。
    signal(pid, rustix::process::Signal::STOP);
    endpoint.enqueue(fx.main(), ModelReply::text("第二轮"));
    fx.send("echo-send", &session, "停住时发的").await;
    let written = fx
        .wait(&session, "written", |s| {
            prompt(s, "停住时发的").is_some_and(|p| p.data["state"] == "written")
        })
        .await;
    let native = prompt(&written, "停住时发的").unwrap().data["native"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let still = fx.peek(&session).await;
    assert_eq!(
        prompt(&still, "停住时发的").unwrap().data["state"],
        "written"
    );
    assert_eq!(endpoint.requests().len(), 1);
    signal(pid, rustix::process::Signal::CONT);
    let landed = fx
        .wait(&session, "landed", |s| {
            prompt(s, "停住时发的").is_some_and(|p| p.data["state"] == "landed")
                && texts(s) == ["第一轮", "第二轮"]
        })
        .await;
    assert_eq!(
        prompt(&landed, "停住时发的").unwrap().data["native"],
        native
    );
    let bs = header(&landed)["process"]["backend_session"]
        .as_str()
        .unwrap()
        .to_owned();
    let transcript = fx.transcript(&bs);
    assert!(transcript.contains(&native));
    fx.close();
}

#[tokio::test]
async fn a_create_that_never_started_a_backend_is_withdrawn_with_one_notice() {
    let fx = Fixture::start("nd13-withdraw", 3_600_000).await;
    let session = fx
        .create("bad-cwd", "/sandbox/project/没有这个目录", "你好")
        .await;
    let snapshot = fx
        .wait(&session, "withdrawn", |s| {
            has_header(s, |h| h["status"] == "withdrawn")
        })
        .await;
    assert!(
        header(&snapshot)["note"]
            .as_str()
            .is_some_and(|n| !n.is_empty())
    );
    assert!(listed(&fx, &session).await.is_none());
    let global = fx.ui().await.subscribe("global").await.unwrap();
    let notices: Vec<&Item> = global
        .items
        .iter()
        .filter(|i| i.kind == "notice" && i.data["session"] == session)
        .collect();
    assert_eq!(notices.len(), 1, "{:#?}", global.items);
    // 没做过不可逆步骤：模型没被请求，后端进程从没起来。
    assert!(fx.scenario.endpoint().requests().is_empty());
    let runs = fx.ui().await.get("runs", Default::default()).await.unwrap();
    assert!(
        runs.items.iter().all(|r| r.data["state"] == "Gone"),
        "{:#?}",
        runs.items
    );
    fx.close();
}

#[tokio::test]
async fn a_create_whose_backend_died_after_the_first_message_was_written_is_partial() {
    let fx = Fixture::start("nd13-partial", 3_600_000).await;
    // 场景故障点：写首条消息之前让后端进程停住，消息写出了、CLI 没读到。
    std::fs::write(
        fx.scenario.root().join("runtime/backend-fault.json"),
        r#"{"contains":"ND-STOP"}"#,
    )
    .unwrap();
    let session = fx
        .create("partial-create", "/sandbox/project", "ND-STOP 你好")
        .await;
    let written = fx
        .wait(&session, "first message written", |s| {
            prompt(s, "ND-STOP 你好").is_some_and(|p| p.data["state"] == "written")
        })
        .await;
    assert_eq!(header(&written)["status"], "preparing");
    let pid = cli_pid(&fx, &written).await;
    signal(pid, rustix::process::Signal::KILL);
    let snapshot = fx
        .wait(&session, "partial", |s| {
            has_header(s, |h| h["status"] == "partial")
        })
        .await;
    let irreversible = header(&snapshot)["irreversible"].to_string();
    assert!(
        irreversible.contains("first") && irreversible.contains("可能已做"),
        "{irreversible}"
    );
    assert_eq!(
        prompt(&snapshot, "ND-STOP 你好").unwrap().data["state"],
        "unknown"
    );
    assert_eq!(
        listed(&fx, &session).await.unwrap().data["status"],
        "partial"
    );
    assert!(fx.scenario.endpoint().requests().is_empty());
    fx.close();
}

#[tokio::test]
async fn an_idle_backend_is_reclaimed_and_the_next_message_resumes_it() {
    let fx = Fixture::start("nd13-idle", 1500).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("第一轮"));
    let session = fx.create("idle-create", "/sandbox/project", "你好").await;
    let first = fx
        .wait(&session, "first turn", |s| {
            texts(s) == ["第一轮"] && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let first_run = header(&first)["process"]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    // 没人订阅这个会话：只经列表看它的后端进程还在不在。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while listed(&fx, &session).await.unwrap().data["process_alive"] == true {
        assert!(tokio::time::Instant::now() < deadline, "never reclaimed");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let runs = fx.ui().await.get("runs", Default::default()).await.unwrap();
    assert!(
        runs.items
            .iter()
            .any(|r| r.data["run"] == first_run && r.data["state"] == "Gone"),
        "{:#?}",
        runs.items
    );
    endpoint.enqueue(fx.main(), ModelReply::text("第二轮"));
    fx.send("idle-send", &session, "继续").await;
    let after = fx
        .wait(&session, "relaunched and answered", |s| {
            texts(s) == ["第一轮", "第二轮"]
        })
        .await;
    assert_ne!(header(&after)["process"]["run"], first_run);
    assert_eq!(prompt(&after, "继续").unwrap().data["state"], "landed");
    // 续接同一个后端会话：第二次请求带着第一轮的历史。
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 2);
    let second = request_text(&requests[1].body);
    assert!(second.contains("你好") && second.contains("第一轮") && second.contains("继续"));
    assert_eq!(
        header(&after)["process"]["backend_session"],
        header(&first)["process"]["backend_session"]
    );
    fx.close();
}

#[tokio::test]
async fn a_backend_with_a_running_background_task_is_not_reclaimed() {
    let fx = Fixture::start("nd13-busy", 1000).await;
    // 只给这个场景放行 Bash（CLI 自己的用户设置），审批台是第 3 步的事。
    std::fs::create_dir_all(fx.scenario.root().join("claude")).unwrap();
    std::fs::write(
        fx.scenario.root().join("claude/settings.json"),
        r#"{"permissions":{"allow":["Bash"]}}"#,
    )
    .unwrap();
    let mut fifo = fx.scenario.fifo("hold").unwrap();
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(
        fx.main(),
        ModelReply::tool(
            "toolu_bg_1",
            "Bash",
            json!({"command": format!("head -n 1 {}", fifo.sandbox_path().display()), "run_in_background": true, "description": "wait for the test"}),
        ),
    );
    endpoint.enqueue(fx.main(), ModelReply::text("后台任务在跑"));
    let session = fx
        .create("busy-create", "/sandbox/project", "跑个后台命令")
        .await;
    let busy = fx
        .wait(&session, "turn done with a background task", |s| {
            texts(s).contains(&"后台任务在跑".to_owned())
                && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    assert_eq!(
        header(&busy)["process"]["drain"]["drain"],
        "busy",
        "{}",
        header(&busy)
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        listed(&fx, &session).await.unwrap().data["process_alive"],
        true,
        "a backend with a running task must not be reclaimed"
    );
    // 任务结束后 CLI 会把结果交给模型；之后没有任务了，闲置到时限就回收。
    endpoint.enqueue(fx.main(), ModelReply::text("收到任务结果"));
    fifo.release("done").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while listed(&fx, &session).await.unwrap().data["process_alive"] == true {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never reclaimed after the task finished"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    fx.close();
}

#[tokio::test]
async fn the_session_keeps_its_backend_process_across_a_daemon_restart() {
    let fx = Fixture::start("nd13-restart", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("重启前"));
    let session = fx
        .create("restart-create", "/sandbox/project", "你好")
        .await;
    let before = fx
        .wait(&session, "first turn", |s| {
            texts(s) == ["重启前"] && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let pid = cli_pid(&fx, &before).await;
    fx.scenario.kill_daemon().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    endpoint.enqueue(fx.main(), ModelReply::text("重启后"));
    fx.send("restart-send", &session, "还在吗").await;
    let after = fx
        .wait(&session, "answered after restart", |s| {
            texts(s) == ["重启前", "重启后"]
        })
        .await;
    assert_eq!(
        header(&after)["process"]["run"],
        header(&before)["process"]["run"]
    );
    assert_eq!(cli_pid(&fx, &after).await, pid);
    assert_eq!(prompt(&after, "还在吗").unwrap().data["state"], "landed");
    fx.close();
}

/// 录制回归的来源：主接缝跑一段真对话（流式文字、工具调用与结果、两个回合），
/// 适配器录下它读到的看守流水；按能力、后端、版本、场景存成夹具，喂对话状态机得到的事实
/// 要和守护进程实际显示的一致。设了 `ND_RECORD_FIXTURE=<目录>` 时把夹具和事实写进去（入库用）。
#[tokio::test]
async fn a_recorded_conversation_replays_through_the_adapter_state_machine() {
    let fx = Fixture::start("nd13-record", 3_600_000).await;
    std::fs::create_dir_all(fx.scenario.root().join("claude")).unwrap();
    std::fs::write(
        fx.scenario.root().join("claude/settings.json"),
        r#"{"permissions":{"allow":["Bash"]}}"#,
    )
    .unwrap();
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(
        fx.main(),
        ModelReply::tool(
            "toolu_rec_1",
            "Bash",
            json!({"command":"echo ND_TOOL_OK","description":"say ok"}),
        ),
    );
    endpoint.enqueue(
        fx.main(),
        ModelReply::streaming_text("命令跑完了：ND_TOOL_OK", 2, 10),
    );
    endpoint.enqueue(fx.main(), ModelReply::streaming_text("第二轮的回答", 2, 10));
    let session = fx
        .create("record-create", "/sandbox/project", "跑个命令")
        .await;
    fx.wait(&session, "first turn", |s| {
        texts(s) == ["命令跑完了：ND_TOOL_OK"]
            && has_header(s, |h| h["process"]["turn_running"] == false)
    })
    .await;
    fx.send("record-send", &session, "第二条").await;
    let snapshot = fx
        .wait(&session, "second turn", |s| {
            texts(s) == ["命令跑完了：ND_TOOL_OK", "第二轮的回答"]
                && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let run = header(&snapshot)["process"]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let raw = std::fs::read_to_string(fx.scenario.root().join(format!("recordings/{run}.jsonl")))
        .unwrap();
    let records: Vec<nd_watchdog_proto::Record> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let meta = nd_watchdog_proto::FixtureMeta {
        format: 1,
        capability: "conversation".into(),
        backend: "claude".into(),
        version: "2.1.289".into(),
        scenario: "stream-tool-two-turns".into(),
    };
    let path = fx.scenario.root().join("out/stream-tool-two-turns.jsonl");
    nd_watchdog_proto::write_fixture(&path, &meta, &records).unwrap();
    let (read_meta, read) = nd_watchdog_proto::read_fixture(&path).unwrap();
    assert_eq!(read_meta, meta);
    let facts: Vec<nd_claude::Convo> = nd_claude::convo::replay(&read)
        .into_iter()
        .flatten()
        .collect();
    // 每条写出的 user 行恰好回显一次，回显的 uuid 就是会话里落地消息的原生编号。
    let written: Vec<&str> = facts
        .iter()
        .filter_map(|f| match f {
            nd_claude::Convo::Written { uuid } => Some(uuid.as_str()),
            _ => None,
        })
        .collect();
    let echoed: Vec<&str> = facts
        .iter()
        .filter_map(|f| match f {
            nd_claude::Convo::Echo { uuid } => Some(uuid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(written, echoed);
    let landed: Vec<&str> = snapshot
        .items
        .iter()
        .filter(|i| i.kind == "prompt")
        .map(|i| i.data["native"].as_str().unwrap())
        .collect();
    assert_eq!(echoed, landed);
    // 完整块与守护进程显示的一致：工具调用、工具结果、两段文字。
    let blocks: Vec<&nd_backend::Item> = facts
        .iter()
        .filter_map(|f| match f {
            nd_claude::Convo::Block { item } => Some(item),
            _ => None,
        })
        .collect();
    assert!(
        blocks
            .iter()
            .any(|b| b.kind == nd_backend::ItemKind::ToolUse && b.text.contains("ND_TOOL_OK"))
    );
    assert!(
        blocks
            .iter()
            .any(|b| b.kind == nd_backend::ItemKind::ToolResult && b.text.contains("ND_TOOL_OK"))
    );
    let block_texts: Vec<&str> = blocks
        .iter()
        .filter(|b| b.kind == nd_backend::ItemKind::Text)
        .map(|b| b.text.as_str())
        .collect();
    assert_eq!(block_texts, texts(&snapshot));
    let ends = facts
        .iter()
        .filter(|f| matches!(f, nd_claude::Convo::TurnEnded { ok: true, .. }))
        .count();
    assert_eq!(ends, 2);
    if let Some(dest) = std::env::var_os("ND_RECORD_FIXTURE") {
        let dest = Path::new(&dest);
        std::fs::create_dir_all(dest).unwrap();
        std::fs::copy(&path, dest.join("stream-tool-two-turns.jsonl")).unwrap();
        std::fs::write(
            dest.join("stream-tool-two-turns.facts.json"),
            serde_json::to_string_pretty(&nd_claude::convo::replay(&read)).unwrap(),
        )
        .unwrap();
    }
    fx.close();
}

#[tokio::test]
async fn a_backend_that_died_is_relaunched_on_the_next_message() {
    let fx = Fixture::start("nd13-died", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("第一轮"));
    let session = fx.create("died-create", "/sandbox/project", "你好").await;
    let first = fx
        .wait(&session, "first turn", |s| {
            texts(s) == ["第一轮"] && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let pid = cli_pid(&fx, &first).await;
    signal(pid, rustix::process::Signal::KILL);
    // 看守记下退出、单元清理完，独占登记放掉租约，会话头才报进程不在。
    fx.wait(&session, "process gone", |s| {
        has_header(s, |h| {
            h["process"]["alive"] == false && h["status"] == "active"
        })
    })
    .await;
    endpoint.enqueue(fx.main(), ModelReply::text("第二轮"));
    fx.send("died-send", &session, "还在吗").await;
    let after = fx
        .wait(&session, "relaunched", |s| texts(s) == ["第一轮", "第二轮"])
        .await;
    assert_ne!(
        header(&after)["process"]["run"],
        header(&first)["process"]["run"]
    );
    assert_eq!(prompt(&after, "还在吗").unwrap().data["state"], "landed");
    assert!(request_text(&endpoint.requests()[1].body).contains("第一轮"));
    fx.close();
}
