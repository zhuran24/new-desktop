//! 主接缝（#22）：总结、`!` 模式、fork 型子代理与只能聊天的降级提示。
//! 同步副本对真守护进程讲 nd-wire；真 CLI 2.1.289、两个 mod、看守、systemd、SQLite，
//! 只换模型端点（离线伪端点）。断言快照、收据和模型端点实际收到的请求。
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
    /// `hook_mod`：false 时钩子 mod 换成一个不报到的空 mod（只有清单），模拟「mod 没装上」。
    async fn start(name: &str, hook_mod: bool) -> Self {
        let config = r#"
[claude]
cli = "/cli"
hook_mod = "{root}/mods/new-desktop"
action_mod = "{root}/mods/new-desktop-actions"
config_dir = "{root}/claude"
inherit_env = false
record = true
hello_timeout_ms = 4000
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
idle_reclaim_ms = 3600000
tick_ms = 50
"#;
        let mut options = ScenarioOptions::new(
            name,
            std::env::var_os("ND_TEST_DAEMON").expect("run scripts/test-scenarios.sh"),
        );
        options.watchdog = Some(std::env::var_os("ND_TEST_WATCHDOG").unwrap().into());
        options.config = config.into();
        options.timeout = Duration::from_secs(20);
        let scenario = Scenario::start(options).await.unwrap();
        let mods = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods");
        for module in ["new-desktop", "new-desktop-actions"] {
            let target = scenario.root().join("mods").join(module);
            if module == "new-desktop" && !hook_mod {
                copy_dir(
                    &mods.join(module).join(".claude-plugin"),
                    &target.join(".claude-plugin"),
                );
                continue;
            }
            copy_dir(&mods.join(module), &target);
        }
        Self { scenario }
    }
    fn root(&self) -> &Path {
        self.scenario.root()
    }
    fn main(&self) -> Route {
        Route::new(None, MODEL)
    }
    async fn ui(&self) -> SyncReplica {
        self.scenario.connect().await.unwrap()
    }
    async fn peek(&self, session: &str) -> Snapshot {
        let mut ui = self.ui().await;
        let snapshot = ui.subscribe(&format!("session/{session}")).await.unwrap();
        let _ = ui.close().await;
        snapshot
    }
    async fn wait(&self, session: &str, what: &str, until: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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
    /// 收据等动作有结果的命令：同步副本等到收据（或时限后查收据）。
    async fn deliver(&self, id: &str, name: &str, args: Value) -> CommandReply {
        self.ui()
            .await
            .command_waiting(
                &Command {
                    id: id.into(),
                    device: "test".into(),
                    name: name.into(),
                    args,
                    expect: json!({}),
                },
                Duration::from_secs(90),
            )
            .await
            .unwrap()
    }
    async fn command(&self, id: &str, name: &str, args: Value, expect: Value) -> CommandReply {
        self.ui()
            .await
            .command(&Command {
                id: id.into(),
                device: "test".into(),
                name: name.into(),
                args,
                expect,
            })
            .await
            .unwrap()
    }
    async fn create(&self, id: &str, text: &str) -> String {
        match self
            .command(
                id,
                "session.create",
                json!({"cwd":"/sandbox/project","text":text,"model":MODEL}),
                json!({}),
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
            .command(
                id,
                "session.send",
                json!({"session":session,"text":text}),
                json!({}),
            )
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
    /// 等一轮结束：回答出现、回合不在跑。
    async fn turn(&self, session: &str, answer: &str) -> Snapshot {
        self.wait(session, &format!("answer {answer}"), |s| {
            texts(s).iter().any(|t| t == answer)
                && header(s)["process"]["turn_running"] == false
                && prompts_settled(s)
        })
        .await
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
fn item<'a>(snapshot: &'a Snapshot, id: &str) -> Option<&'a Item> {
    snapshot.items.iter().find(|i| i.id == id)
}
fn texts(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .items
        .iter()
        .filter(|i| i.kind == "text" && i.data["complete"] == true)
        .map(|i| i.data["text"].as_str().unwrap().to_owned())
        .collect()
}
fn prompts_settled(snapshot: &Snapshot) -> bool {
    snapshot
        .items
        .iter()
        .filter(|i| i.kind == "prompt")
        .all(|i| i.data["state"] == "landed")
}
fn done_value(reply: &CommandReply) -> &Value {
    match reply {
        CommandReply::Receipt {
            receipt: Receipt::Done { value },
        } => value,
        other => panic!("expected a Done receipt: {other:?}"),
    }
}
fn request_text(body: &Value) -> String {
    body["messages"].to_string()
}

#[tokio::test]
async fn bang_runs_one_command_without_a_turn_and_the_next_request_reads_its_output() {
    let fx = Fixture::start("nd22-bang", true).await;
    std::fs::create_dir_all(fx.root().join("project/sub")).unwrap();
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("在"));
    let session = fx.create("bang-create", "你好").await;
    fx.turn(&session, "在").await;
    let before = endpoint.requests().len();

    // 要审批的命令（cd、变量）：只自动批准守护进程刚派发、原文一致的这一条。
    let first = fx
        .deliver(
            "bang-1",
            "session.shell",
            json!({"session":session,"command":"cd sub && export ND_VAR=kept && echo FIRST"}),
        )
        .await;
    let first = done_value(&first);
    assert_eq!(first["exit"], 0, "{first}");
    assert!(
        first["stdout"].as_str().unwrap().contains("FIRST"),
        "{first}"
    );
    assert_eq!(first["appended"], true, "{first}");
    let second = fx
        .deliver(
            "bang-2",
            "session.shell",
            json!({"session":session,"command":"pwd; echo \"VAR=[$ND_VAR]\"; exit 3"}),
        )
        .await;
    let second = done_value(&second).clone();
    assert_eq!(second["exit"], 3, "{second}");
    let output = format!("{}{}", second["stdout"], second["stderr"]);
    // 只延续工作目录，不延续环境变量。
    assert!(output.contains("/sandbox/project/sub"), "{second}");
    assert!(output.contains("VAR=[]"), "{second}");
    // `!` 本身不起回合：没有新的模型请求。
    assert_eq!(
        endpoint.requests().len(),
        before,
        "a bang must not query the model"
    );
    let snapshot = fx.peek(&session).await;
    let shown = item(&snapshot, "invoke/bang-2").expect("shell item");
    assert_eq!(shown.kind, "shell");
    assert_eq!(shown.data["state"], "done");
    assert_eq!(shown.data["command"], "pwd; echo \"VAR=[$ND_VAR]\"; exit 3");
    assert!(
        snapshot.items.iter().all(|i| i.kind != "asked"),
        "the dispatched command must not wait for an answer: {:#?}",
        snapshot.items
    );

    // 下一次模型请求读得到命令和输出。
    endpoint.enqueue(fx.main(), ModelReply::text("看到了"));
    fx.send("bang-send", &session, "刚才的输出是什么").await;
    fx.turn(&session, "看到了").await;
    let last = endpoint.requests().last().unwrap().body.clone();
    let text = request_text(&last);
    assert!(
        text.contains("<bash-input>cd sub && export ND_VAR=kept && echo FIRST</bash-input>"),
        "{text}"
    );
    assert!(text.contains("FIRST"), "{text}");
    assert!(text.contains("VAR=[]"), "{text}");

    // 模型自己要跑同一条原文的 Bash：不是守护进程派发的，不自动批准，等界面回答。
    endpoint.enqueue(
        fx.main(),
        ModelReply::tool(
            "toolu_model_same",
            "Bash",
            json!({"command":"cd sub && export ND_VAR=kept && echo FIRST","description":"model"}),
        ),
    );
    fx.send("bang-model", &session, "你自己也跑一下").await;
    let asked = fx
        .wait(&session, "the model's Bash asks for approval", |s| {
            s.items
                .iter()
                .any(|i| i.kind == "asked" && i.data["raw"]["tool_name"] == "Bash")
        })
        .await;
    let waiting = endpoint.requests().len();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        endpoint.requests().len(),
        waiting,
        "the model's own Bash was answered without the owner: {:#?}",
        asked.items
    );
    fx.close();
}
