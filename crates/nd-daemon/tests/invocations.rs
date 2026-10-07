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
    /// 真窗口（私有 KWin）里在产品输入框提交 `text`，见 nd-desktop/tests/native_chat.py 的 invoke 模式。
    async fn native(&self, session: &str, text: &str, expect: &str) -> Value {
        let desktop = std::env::var_os("ND_TEST_DESKTOP").expect("run scripts/test-scenarios.sh");
        let output = self.root().join(format!("native-{expect}"));
        let result = tokio::process::Command::new("python")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
            .arg("--desktop")
            .arg(desktop)
            .arg("--socket")
            .arg(self.root().join("runtime/nd.sock"))
            .arg("--output")
            .arg(&output)
            .arg("--session")
            .arg(session)
            .arg("--invoke")
            .arg(text)
            .arg("--expect")
            .arg(expect)
            .output()
            .await
            .unwrap();
        if let Some(keep) = std::env::var_os("ND_NATIVE_OUTPUT") {
            let target = Path::new(&keep).join(format!("invoke-{expect}"));
            let _ = std::fs::create_dir_all(&target);
            for entry in std::fs::read_dir(&output).into_iter().flatten().flatten() {
                let _ = std::fs::copy(entry.path(), target.join(entry.file_name()));
            }
        }
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap()
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

    // 回合进行中输入的 `!`：和终端一样，等这一回合结束再跑。
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("慢回答"));
    fx.send("bang-busy-send", &session, "说慢一点").await;
    fx.wait(&session, "a turn is running", |s| {
        header(s)["process"]["turn_running"] == true
    })
    .await;
    let mut ui = fx.ui().await;
    let busy = Command {
        id: "bang-busy".into(),
        device: "test".into(),
        name: "session.shell".into(),
        args: json!({"session":session,"command":"echo AFTER_TURN"}),
        expect: json!({}),
    };
    let pending = tokio::spawn(async move {
        ui.command_waiting(&busy, Duration::from_secs(90))
            .await
            .unwrap()
    });
    fx.wait(&session, "the bang waits for the turn", |s| {
        item(s, "invoke/bang-busy").is_some_and(|i| i.data["state"] == "waiting_turn")
    })
    .await;
    gate.release();
    let after_turn = pending.await.unwrap();
    assert!(
        done_value(&after_turn)["stdout"]
            .as_str()
            .is_some_and(|o| o.contains("AFTER_TURN")),
        "{after_turn:?}"
    );
    fx.turn(&session, "慢回答").await;

    // 真窗口：在产品输入框打 `!` 命令提交，经同一条 nd-wire 命令跑完，输入框被守护进程清空。
    let verdict = fx.native(&session, "!echo NATIVE_BANG", "cleared").await;
    assert_eq!(verdict["pass"], true, "{verdict}");
    let native = fx
        .wait(&session, "the native bang finished", |s| {
            s.items.iter().any(|i| {
                i.kind == "shell"
                    && i.data["command"] == "echo NATIVE_BANG"
                    && i.data["state"] == "done"
                    && i.data["stdout"]
                        .as_str()
                        .is_some_and(|o| o.contains("NATIVE_BANG"))
            })
        })
        .await;
    assert_eq!(draft(&native)["text"], "");

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

fn prompt_id(snapshot: &Snapshot, text: &str, nth: usize) -> String {
    let mut prompts: Vec<&Item> = snapshot
        .items
        .iter()
        .filter(|i| i.kind == "prompt" && i.data["text"] == text)
        .collect();
    prompts.sort_by_key(|i| i.data["seq"].as_u64());
    prompts[nth].data["message"].as_str().unwrap().to_owned()
}
fn draft(snapshot: &Snapshot) -> &Value {
    &item(snapshot, "draft").expect("draft").data
}
fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

impl Fixture {
    /// 一轮对话：排好回答、发出、等回答。
    async fn round(&self, session: &str, id: &str, text: &str, answer: &str) -> Snapshot {
        self.scenario
            .endpoint()
            .enqueue(self.main(), ModelReply::text(answer));
        self.send(id, session, text).await;
        self.turn(session, answer).await
    }
    async fn edit_draft(&self, session: &str, id: &str, version: u64, text: &str) {
        let reply = self
            .command(
                id,
                "session.draft.update",
                json!({"session":session,"text":text}),
                json!({"draft_version":version}),
            )
            .await;
        assert!(
            matches!(
                reply,
                CommandReply::Receipt {
                    receipt: Receipt::Done { .. }
                }
            ),
            "{reply:?}"
        );
    }
}

#[tokio::test]
async fn summarize_from_here_compacts_only_the_selected_range_and_returns_the_prompt_to_the_draft()
{
    let fx = Fixture::start("nd22-from", true).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("答一"));
    let session = fx.create("from-create", "第一条").await;
    fx.turn(&session, "答一").await;
    fx.round(&session, "from-2", "重复的提示", "答二").await;
    fx.round(&session, "from-3", "第三条", "答三").await;
    let ready = fx.round(&session, "from-4", "重复的提示", "答四").await;
    // 选第二次出现的「重复的提示」：同一原文按次序定位。
    let chosen = prompt_id(&ready, "重复的提示", 1);
    assert_eq!(chosen, "from-4");
    fx.edit_draft(&session, "from-draft", 0, "还没发的草稿")
        .await;
    let before = endpoint.requests().len();
    endpoint.enqueue(fx.main(), ModelReply::text("范围摘要"));
    let reply = fx
        .deliver(
            "from-sum",
            "session.compact",
            json!({"session":session,"message":chosen,"scope":"from"}),
        )
        .await;
    let value = done_value(&reply).clone();
    // 摘要器只拿到所选范围：第二次的「重复的提示」及之后，前面的原行不交给它。
    let requests = endpoint.requests();
    assert_eq!(requests.len(), before + 1, "exactly one summarizer request");
    let summarizer = request_text(&requests[before].body);
    assert_eq!(count(&summarizer, "重复的提示"), 1, "{summarizer}");
    assert!(!summarizer.contains("第一条"), "{summarizer}");
    assert!(!summarizer.contains("第三条"), "{summarizer}");
    // 「从这里总结」：所选提示原文回到输入框；被替换的草稿另存、不丢。
    assert_eq!(value["draft"]["text"], "重复的提示", "{value}");
    let after = fx.peek(&session).await;
    assert_eq!(draft(&after)["text"], "重复的提示");
    let saved = draft(&after)["saved"].as_array().unwrap();
    assert!(
        saved.iter().any(|s| s["text"] == "还没发的草稿"),
        "{saved:?}"
    );
    let shown = item(&after, "invoke/from-sum").expect("compact item");
    assert_eq!(shown.kind, "compact");
    assert_eq!(shown.data["state"], "done");
    let summarized = item(&after, "lineage").unwrap().data["summarized"].clone();
    assert_eq!(summarized, json!(["from-4"]), "{summarized}");

    // 之后的请求：范围外的原行还在，所选范围换成了摘要。
    let next = fx.round(&session, "from-5", "继续", "答五").await;
    let text = request_text(&endpoint.requests().last().unwrap().body);
    assert!(text.contains("第一条") && text.contains("第三条"), "{text}");
    assert!(text.contains("范围摘要"), "{text}");
    assert_eq!(
        count(&text, "重复的提示"),
        1,
        "the summarized prompt is gone: {text}"
    );
    // 已被总结的提示不能再选。
    let again = fx
        .deliver(
            "from-again",
            "session.compact",
            json!({"session":session,"message":"from-4","scope":"up_to"}),
        )
        .await;
    assert!(
        matches!(&again, CommandReply::Receipt { receipt: Receipt::Rejected { code, .. } } if code == "precondition"),
        "{again:?}"
    );
    assert!(texts(&next).contains(&"答五".to_owned()));
    fx.close();
}

#[tokio::test]
async fn summarize_up_to_here_compacts_what_came_before_and_leaves_the_draft_empty() {
    let fx = Fixture::start("nd22-upto", true).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("答甲"));
    let session = fx.create("upto-create", "甲提示").await;
    let first = fx.turn(&session, "答甲").await;
    let head = prompt_id(&first, "甲提示", 0);
    fx.round(&session, "upto-2", "乙提示", "答乙").await;
    fx.round(&session, "upto-3", "丙提示", "答丙").await;
    // 第一条之前没有可总结的。
    let nothing = fx
        .deliver(
            "upto-head",
            "session.compact",
            json!({"session":session,"message":head,"scope":"up_to"}),
        )
        .await;
    assert!(
        matches!(&nothing, CommandReply::Receipt { receipt: Receipt::Rejected { code, .. } } if code == "precondition"),
        "{nothing:?}"
    );
    fx.edit_draft(&session, "upto-draft", 0, "写了一半").await;
    let before = endpoint.requests().len();
    endpoint.enqueue(fx.main(), ModelReply::text("前文摘要"));
    let reply = fx
        .deliver(
            "upto-sum",
            "session.compact",
            json!({"session":session,"message":"upto-3","scope":"up_to"}),
        )
        .await;
    let value = done_value(&reply).clone();
    let requests = endpoint.requests();
    assert_eq!(requests.len(), before + 1, "exactly one summarizer request");
    let summarizer = request_text(&requests[before].body);
    assert!(
        summarizer.contains("甲提示") && summarizer.contains("乙提示"),
        "{summarizer}"
    );
    assert!(!summarizer.contains("丙提示"), "{summarizer}");
    // 「总结到这里」：输入框留空；原来写了一半的另存。
    assert_eq!(value["draft"]["text"], "", "{value}");
    let after = fx.peek(&session).await;
    assert_eq!(draft(&after)["text"], "");
    assert!(
        draft(&after)["saved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["text"] == "写了一半")
    );
    fx.round(&session, "upto-4", "接着说", "答丁").await;
    let text = request_text(&endpoint.requests().last().unwrap().body);
    assert!(
        text.contains("丙提示") && text.contains("前文摘要"),
        "{text}"
    );
    assert!(
        !text.contains("甲提示") && !text.contains("乙提示"),
        "{text}"
    );
    fx.close();
}

#[tokio::test]
async fn a_prompt_the_cli_no_longer_holds_is_not_compacted_and_the_reason_is_shown() {
    let fx = Fixture::start("nd22-gone", true).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("答甲"));
    let session = fx.create("gone-create", "甲提示").await;
    let first = fx.turn(&session, "答甲").await;
    let head = prompt_id(&first, "甲提示", 0);
    fx.round(&session, "gone-2", "乙提示", "答乙").await;
    // 终端式的整段 /compact：CLI 自己把之前的对话都换成摘要，守护进程不知道这一步。
    endpoint.enqueue(fx.main(), ModelReply::text("整段摘要"));
    let before = endpoint.requests().len();
    fx.send("gone-compact", &session, "/compact").await;
    endpoint
        .wait_for_requests(&fx.main(), before + 1, Duration::from_secs(30))
        .await
        .unwrap();
    fx.wait(&session, "the CLI compaction finished", |s| {
        header(s)["process"]["turn_running"] == false
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = endpoint.requests().len();
    let draft_before = draft(&fx.peek(&session).await).clone();
    let reply = fx
        .deliver(
            "gone-sum",
            "session.compact",
            json!({"session":session,"message":head,"scope":"from"}),
        )
        .await;
    let CommandReply::Receipt {
        receipt: Receipt::Rejected { code, now },
    } = &reply
    else {
        panic!("expected a rejection: {reply:?}");
    };
    assert_eq!(code, "anchor_gone", "{now}");
    assert!(now["reason"].as_str().unwrap().contains("找不到"), "{now}");
    // 定位不到就不压缩：没有摘要请求，草稿不动，条目写明原因。
    assert_eq!(endpoint.requests().len(), before, "nothing was summarized");
    let after = fx.peek(&session).await;
    assert_eq!(draft(&after), &draft_before);
    let shown = item(&after, "invoke/gone-sum").expect("compact item");
    assert_eq!(shown.data["state"], "rejected");
    assert!(shown.fallback.text.contains("找不到"), "{shown:?}");
    fx.close();
}

#[tokio::test]
async fn subtask_dispatches_a_fork_subagent_that_carries_the_parent_conversation() {
    let fx = Fixture::start("nd22-subtask", true).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("父回答"));
    let session = fx.create("sub-create", "父对话标记 PARENT_MARK").await;
    fx.turn(&session, "父回答").await;
    // fork 子代理的请求带着动态 agent id；它完成后 CLI 自己把结果交给主对话。
    endpoint.enqueue_any_agent(MODEL, ModelReply::text("子任务完成"));
    endpoint.enqueue(fx.main(), ModelReply::text("收到子任务结果"));
    let reply = fx
        .deliver(
            "sub-1",
            "session.subtask",
            json!({"session":session,"prompt":"去查一下 FORK_TASK"}),
        )
        .await;
    let value = done_value(&reply).clone();
    let agent = value["agent"].as_str().expect("agent id").to_owned();
    assert!(!agent.is_empty(), "{value}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let request = loop {
        if let Some(r) = endpoint
            .requests()
            .into_iter()
            .find(|r| r.route.agent.as_deref() == Some(agent.as_str()))
        {
            break r;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the fork never asked the model"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let text = request_text(&request.body);
    assert!(
        text.contains("PARENT_MARK"),
        "the fork carries the parent context: {text}"
    );
    assert!(text.contains("FORK_TASK"), "{text}");
    // 子代理在后台跑完：后台任务表回到空。
    let after = fx
        .wait(&session, "the fork finished in the background", |s| {
            header(s)["process"]["drain"]["drain"] == "drained"
        })
        .await;
    let shown = item(&after, "invoke/sub-1").expect("subtask item");
    assert_eq!(shown.kind, "subtask");
    assert_eq!(shown.data["state"], "done");
    assert_eq!(shown.data["agent"], agent);
    fx.close();
}

#[tokio::test]
async fn without_the_hook_mod_the_header_lists_what_cannot_be_used_and_chat_still_works() {
    let fx = Fixture::start("nd22-chatonly", false).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("只能聊天也能答"));
    let session = fx.create("chatonly-create", "你好").await;
    let snapshot = fx.turn(&session, "只能聊天也能答").await;
    let degraded = header(&snapshot)["degraded"].clone();
    assert!(
        degraded["why"].as_str().unwrap().contains("new-desktop"),
        "{degraded}"
    );
    let unavailable: Vec<&str> = degraded["unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for feature in [
        "退役",
        "派 Codex 子代理",
        "给子代理直接发消息",
        "总结",
        "! 模式",
        "fork 型子代理",
        "设置行",
        "当前模型操作转接来的任务（只能看结果）",
    ] {
        assert!(
            unavailable.contains(&feature),
            "{feature} missing: {degraded}"
        );
    }
    let first = prompt_id(&snapshot, "你好", 0);
    for (id, name, args) in [
        (
            "co-shell",
            "session.shell",
            json!({"session":session,"command":"pwd"}),
        ),
        (
            "co-sub",
            "session.subtask",
            json!({"session":session,"prompt":"x"}),
        ),
        (
            "co-sum",
            "session.compact",
            json!({"session":session,"message":first,"scope":"from"}),
        ),
    ] {
        let reply = fx.deliver(id, name, args).await;
        assert!(
            matches!(&reply, CommandReply::Receipt { receipt: Receipt::Rejected { code, now } } if code == "unsupported" && now["why"].as_str().is_some_and(|w| w.contains("new-desktop"))),
            "{name}: {reply:?}"
        );
    }
    // 真窗口：会话头列出用不了的功能；输入框里的 `!` 不发出，正文留着。
    let verdict = fx.native(&session, "!pwd", "refused").await;
    let notice = verdict["notice"]["degraded"].as_str().unwrap();
    assert!(
        notice.contains("总结") && notice.contains("! 模式"),
        "{verdict}"
    );
    assert!(
        fx.peek(&session)
            .await
            .items
            .iter()
            .all(|i| i.kind != "shell"),
        "a refused bang must not reach the daemon"
    );
    // 聊天照常。
    fx.round(&session, "chatonly-2", "再问一句", "还能答").await;
    fx.close();
}

#[tokio::test]
async fn a_bang_running_across_a_daemon_restart_settles_once_from_the_mod_result() {
    let fx = Fixture::start("nd22-bang-restart", true).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("在"));
    let session = fx.create("br-create", "你好").await;
    fx.turn(&session, "在").await;
    let mut fifo = fx.scenario.fifo("hold").unwrap();
    let command = format!(
        "head -n 1 {} && echo AFTER_RESTART >> /sandbox/project/ran.log",
        fifo.sandbox_path().display()
    );
    let ui_command = Command {
        id: "br-1".into(),
        device: "test".into(),
        name: "session.shell".into(),
        args: json!({"session":session,"command":command}),
        expect: json!({}),
    };
    let mut ui = fx.ui().await;
    let waiting = tokio::spawn(async move {
        ui.command_waiting(&ui_command, Duration::from_secs(5))
            .await
    });
    let running = fx
        .wait(&session, "the bang is running", |s| {
            item(s, "invoke/br-1").is_some_and(|i| i.data["state"] == "running")
        })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    fx.scenario.kill_daemon().unwrap();
    assert_ne!(
        fx.peek(&session).await.epoch,
        running.epoch,
        "the daemon really restarted"
    );
    let _ = waiting.await;
    // 接回之后命令还在跑：重启后的对账只能按操作 id 问到「还在跑」，不能当成不明。
    fx.wait(&session, "recovered while the bang still runs", |s| {
        header(s)["recovering"] == false
            && item(s, "invoke/br-1").is_some_and(|i| i.data["state"] == "running")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        item(&fx.peek(&session).await, "invoke/br-1").unwrap().data["state"],
        "running"
    );
    fifo.release("go").unwrap();
    // 重启后按操作 id 问动作 mod，拿到这条命令的结论；收据只落一次。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let receipt = loop {
        let mut ui = fx.ui().await;
        if let Ok(nd_wire::ReceiptLookup::Found { receipt }) = ui.receipt("br-1").await {
            break receipt;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no receipt after restart"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let Receipt::Done { value } = &receipt else {
        panic!("expected Done: {receipt:?}");
    };
    assert_eq!(value["exit"], 0, "{value}");
    let ran = std::fs::read_to_string(fx.root().join("project/ran.log")).unwrap();
    assert_eq!(ran.matches("AFTER_RESTART").count(), 1, "ran exactly once");
    let after = fx.peek(&session).await;
    assert_eq!(item(&after, "invoke/br-1").unwrap().data["state"], "done");
    // 同 id 重发拿原收据，不再跑。
    let again = fx
        .deliver(
            "br-1",
            "session.shell",
            json!({"session":session,"command":command}),
        )
        .await;
    assert_eq!(again, CommandReply::Receipt { receipt });
    let ran = std::fs::read_to_string(fx.root().join("project/ran.log")).unwrap();
    assert_eq!(ran.matches("AFTER_RESTART").count(), 1);
    fx.close();
}
