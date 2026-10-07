//! 引擎崩溃矩阵（窄接缝一）：对每个（操作，场景），在三处故障点
//! （提交前；提交后、发件交出前；发件交出后、结果入账前）的每一次提交上杀掉引擎，
//! 换新实例重开、跑完，断言终态、原生步骤至多一次、代持有去处、没有漏掉的进程。
//! 每次重跑都跑两遍比对（纯度）。这里覆盖第 2 步的三个操作：新建、闲置回收、按需拉起。
mod support;
use nd_backend::SessionId;
use nd_session::{
    EngineConfig, Fault, Faults,
    scripted::{ActKind, Reply},
    session_id_for,
};
use nd_wire::Snapshot;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;

/// 在某一处故障点的第 k 次经过时「崩溃」。
struct CrashAt {
    point: Fault,
    nth: usize,
    seen: AtomicUsize,
    fired: AtomicBool,
}
impl Faults for CrashAt {
    fn crash(&self, _: &SessionId, point: Fault) -> bool {
        if point != self.point || self.fired.load(Ordering::Acquire) {
            return false;
        }
        if self.seen.fetch_add(1, Ordering::AcqRel) + 1 == self.nth {
            self.fired.store(true, Ordering::Release);
            return true;
        }
        false
    }
}

/// 场景的一步：一条命令（可重发，同 id）和它之后要等到的状态。
struct Step {
    command: nd_wire::Command,
    until: fn(&Snapshot) -> bool,
}

struct Scenario {
    name: &'static str,
    uploads: Vec<Vec<u8>>,
    auto_title: bool,
    script: Vec<(ActKind, Reply)>,
    idle_ms: u64,
    steps: Vec<Step>,
    check: fn(&Harness, &Snapshot),
}

fn config_for(idle_ms: u64, auto_title: bool, faults: Option<Arc<dyn Faults>>) -> EngineConfig {
    EngineConfig {
        idle_reclaim: Duration::from_millis(idle_ms),
        auto_title,
        faults,
        ..config()
    }
}

/// 跑一遍；`crash` 为 None 是对照。返回终态快照与故障是否真的触发过。
async fn run(scenario: &Scenario, crash: Option<(Fault, usize)>) -> (Harness, Snapshot, bool) {
    let fault = crash.map(|(point, nth)| {
        Arc::new(CrashAt {
            point,
            nth,
            seen: AtomicUsize::new(0),
            fired: AtomicBool::new(false),
        })
    });
    let mut h = Harness::new(config_for(
        scenario.idle_ms,
        scenario.auto_title,
        fault.clone().map(|f| f as Arc<dyn Faults>),
    ))
    .await;
    let blobs = nd_store::Blobs::open(h.dir.path().join("blobs"), h.store.clone()).unwrap();
    for bytes in &scenario.uploads {
        blobs.put(bytes).unwrap();
    }
    for (kind, reply) in &scenario.script {
        h.adapter.script(kind.clone(), reply.clone());
    }
    let session = session_id_for(&scenario.steps[0].command.id);
    let fired = || {
        fault
            .as_ref()
            .is_some_and(|f| f.fired.load(Ordering::Acquire))
    };
    let mut restarted = false;
    let mut index = 0;
    while index < scenario.steps.len() {
        let step = &scenario.steps[index];
        // 与同步副本一样：仅明确未受理的 unavailable 可同 id 重试。
        let retry_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while matches!(
            h.sessions.execute(&step.command).await,
            Some(nd_wire::CommandReply::Unavailable { .. })
        ) {
            if fired() && !restarted {
                break;
            }
            assert!(
                tokio::time::Instant::now() < retry_deadline,
                "recovery gate stayed closed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut reached = false;
        while tokio::time::Instant::now() < deadline {
            if fired() && !restarted {
                break;
            }
            let snapshot = h.snapshot(&session);
            if (step.until)(&snapshot) {
                reached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if fired() && !restarted {
            h = h
                .restart(config_for(scenario.idle_ms, scenario.auto_title, None))
                .await;
            restarted = true;
            // 界面按同一命令 id 重发：已提交的回原收据，没提交的这次才受理。
            continue;
        }
        assert!(
            reached,
            "{}: step {index} never reached; crash {crash:?}; {:#?}",
            scenario.name,
            h.snapshot(&session).items
        );
        index += 1;
    }
    // 等到静止：没有进行中的操作，适配器里的活进程与会话头一致（闲置回收可能还在收尾）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = h.snapshot(&session);
        let header = header(&snapshot).clone();
        let alive = header["process"]["alive"] == true;
        if header["op"].is_null() && h.adapter.live().len() == usize::from(alive) {
            return (h, snapshot, fired());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{}: never settled; crash {crash:?}: {header}",
            scenario.name
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn applied_counts(h: &Harness) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for step in h.adapter.applied() {
        *counts.entry(step).or_default() += 1;
    }
    counts
}

/// 每个原生步骤至多生效一次；没有进行中的操作；进程与会话状态一致。
fn common(scenario: &Scenario, h: &Harness, snapshot: &Snapshot, crash: Option<(Fault, usize)>) {
    for (step, count) in applied_counts(h) {
        assert_eq!(
            count, 1,
            "{}: {step} applied {count} times; crash {crash:?}",
            scenario.name
        );
    }
    let header = header(snapshot);
    assert!(
        header["op"].is_null(),
        "{}: op left running: {header}",
        scenario.name
    );
    let alive = header["process"]["alive"] == true;
    assert_eq!(
        h.adapter.live().len(),
        usize::from(alive),
        "{}: live processes {:?} vs header {header}; crash {crash:?}",
        scenario.name,
        h.adapter.live()
    );
    (scenario.check)(h, snapshot);
}

async fn matrix(scenario: Scenario) {
    let (h, snapshot, _) = run(&scenario, None).await;
    common(&scenario, &h, &snapshot, None);
    drop(h);
    let mut runs = 0;
    for point in [Fault::BeforeCommit, Fault::AfterCommit, Fault::AfterHandoff] {
        for nth in 1.. {
            assert!(nth < 80, "{}: too many commits", scenario.name);
            let (h, snapshot, fired) = run(&scenario, Some((point, nth))).await;
            common(&scenario, &h, &snapshot, Some((point, nth)));
            runs += 1;
            if !fired {
                // 这一处已经越过最后一次提交。
                eprintln!(
                    "{}: {point:?} covered {} commit points",
                    scenario.name,
                    nth - 1
                );
                break;
            }
        }
    }
    assert!(runs >= 6, "{}: only {runs} runs", scenario.name);
}

fn status_is(s: &Snapshot, status: &str) -> bool {
    s.items
        .iter()
        .any(|i| i.id == "header" && i.data["status"] == status && i.data["op"].is_null())
}

fn create(id: &str, text: &str) -> nd_wire::Command {
    command(
        id,
        "session.create",
        json!({"cwd":"/proj","text":text,"model":"claude-haiku-4-5"}),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn draft_conflict_and_send_consumption_recover_at_every_commit_point() {
    let session = session_id_for("matrix-draft-create");
    let update = |id: &str, version: u64, text: &str| nd_wire::Command {
        id: id.into(),
        device: id.into(),
        name: "session.draft.update".into(),
        args: json!({"session":session,"text":text}),
        expect: json!({"draft_version":version}),
    };
    let send = nd_wire::Command {
        id: "matrix-draft-send".into(),
        device: "a".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"胜出的草稿"}),
        expect: json!({"draft_version":1}),
    };
    matrix(Scenario {
        name: "send/draft-conflict-consume-retry",
        auto_title: false,
        uploads: vec![],
        script: vec![],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-draft-create", "你好"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: update("matrix-draft-a", 0, "胜出的草稿"),
                until: |s| item(s, "draft").data["version"] == 1,
            },
            Step {
                command: update("matrix-draft-b", 0, "落败稿"),
                until: |s| {
                    item(s, "draft").data["saved"]
                        .as_array()
                        .is_some_and(|a| a.len() == 1)
                },
            },
            Step {
                command: send.clone(),
                until: |s| prompt(s, "胜出的草稿").is_some_and(|i| i.data["state"] == "landed"),
            },
            Step {
                command: update("matrix-draft-new", 2, "后来编辑"),
                until: |s| item(s, "draft").data["version"] == 3,
            },
            Step {
                command: send,
                until: |s| item(s, "draft").data["text"] == "后来编辑",
            },
        ],
        check: |h, s| {
            let d = &item(s, "draft").data;
            assert_eq!(d["text"], "后来编辑");
            assert_eq!(d["version"], 3);
            assert_eq!(d["saved"].as_array().unwrap().len(), 1);
            assert_eq!(d["saved"][0]["text"], "落败稿");
            assert_eq!(applied_counts(h).get("send:胜出的草稿"), Some(&1));
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn create_reaches_active_or_is_compensated_at_every_commit_point() {
    matrix(Scenario {
        name: "create/ok",
        auto_title: false,
        uploads: vec![],
        script: vec![],
        idle_ms: 3_600_000,
        steps: vec![Step {
            command: create("matrix-create", "你好"),
            until: |s| status_is(s, "active"),
        }],
        check: |h, s| {
            assert_eq!(prompt(s, "你好").unwrap().data["state"], "landed");
            assert_eq!(applied_counts(h).get("send:你好"), Some(&1));
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_cannot_start_is_withdrawn_at_every_commit_point() {
    matrix(Scenario {
        name: "create/open-fails",
        auto_title: false,
        uploads: vec![],
        script: vec![(ActKind::Open, Reply::Fail("工作目录不存在".into()))],
        idle_ms: 3_600_000,
        steps: vec![Step {
            command: create("matrix-create-fail", "你好"),
            until: |s| status_is(s, "withdrawn"),
        }],
        check: |h, _| assert!(h.adapter.applied().is_empty()),
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_whose_first_message_is_unknown_is_partial_at_every_commit_point() {
    matrix(Scenario {
        name: "create/partial",
        auto_title: false,
        uploads: vec![],
        script: vec![(ActKind::Send, Reply::Unknown("进程退出，没等到回显".into()))],
        idle_ms: 3_600_000,
        steps: vec![
            Step { command: create("matrix-create-partial", "你好"), until: |s| status_is(s, "partial") },
            Step { command: command("after-partial", "session.send", json!({
                "session":session_id_for("matrix-create-partial"), "text":"cannot be held forever"
            })), until: |s| status_is(s, "partial") },
        ],
        check: |h, s| {
            assert!(matches!(receipt(h, "after-partial"), nd_wire::Receipt::Rejected { ref code, .. } if code == "precondition"));
            assert!(header(s)["irreversible"].to_string().contains("first"));
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_reclaim_then_on_demand_launch_deliver_the_held_message_once_at_every_commit_point() {
    let session = session_id_for("matrix-idle");
    matrix(Scenario {
        name: "reclaim+launch",
        auto_title: false,
        uploads: vec![],
        script: vec![],
        idle_ms: 60,
        steps: vec![
            Step {
                command: create("matrix-idle", "你好"),
                until: |s| status_is(s, "active") && header(s)["process"]["alive"] == false,
            },
            Step {
                command: command(
                    "matrix-idle-send",
                    "session.send",
                    json!({"session": session, "text": "又来了"}),
                ),
                until: |s| prompt(s, "又来了").is_some_and(|p| p.data["state"] == "landed"),
            },
        ],
        check: |h, _| {
            let counts = applied_counts(h);
            assert_eq!(counts.get("send:又来了"), Some(&1));
            assert_eq!(
                counts.keys().filter(|k| k.starts_with("open:")).count(),
                2,
                "{counts:?}"
            );
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn confirmed_loss_and_user_resend_are_atomic_at_every_commit_point() {
    use sha2::{Digest, Sha256};
    let bytes = b"resend attachment";
    let blob = format!("{:x}", Sha256::digest(bytes));
    let attachments =
        json!([{"blob":blob,"name":"resend.txt","media_type":"text/plain","size":bytes.len()}]);
    let session = session_id_for("matrix-resend");
    matrix(Scenario {
        name: "send/lost-and-user-resend",
        auto_title: false,
        uploads: vec![bytes.to_vec()],
        script: vec![
            (ActKind::Send, Reply::Ok),
            (ActKind::Send, Reply::Lost("证实未消费".into())),
        ],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-resend", "开始"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "matrix-lost",
                    "session.send",
                    json!({"session":session,"text":"重发的消息","attachments":attachments}),
                ),
                until: |s| {
                    s.items.iter().any(|i| {
                        i.data["message"] == "matrix-lost" && i.data["state"] == "not_delivered"
                    })
                },
            },
            Step {
                command: command(
                    "matrix-resend-once",
                    "session.resend",
                    json!({"session":session,"message":"matrix-lost"}),
                ),
                until: |s| {
                    s.items.iter().any(|i| {
                        i.data["message"] == "matrix-resend-once" && i.data["state"] == "landed"
                    })
                },
            },
        ],
        check: |h, s| {
            assert_eq!(applied_counts(h).get("send:重发的消息"), Some(&1));
            for id in ["matrix-lost", "matrix-resend-once"] {
                let item = s.items.iter().find(|i| i.data["message"] == id).unwrap();
                assert_eq!(item.data["attachments"][0]["name"], "resend.txt");
            }
            for (_, act) in h.adapter.received() {
                if let nd_backend::Act::Send { msg, .. } = act
                    && msg.text == "重发的消息"
                {
                    assert_eq!(msg.attachments.len(), 1);
                    assert_eq!(msg.attachments[0].name, "resend.txt");
                    assert_eq!(msg.attachments[0].size, 17);
                }
            }
            assert_eq!(
                s.items
                    .iter()
                    .find(|i| i.data["message"] == "matrix-lost")
                    .unwrap()
                    .data["state"],
                "resent"
            );
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachment_create_and_send_recover_at_every_commit_point() {
    use sha2::{Digest, Sha256};
    let bytes = b"matrix attachment";
    let blob = format!("{:x}", Sha256::digest(bytes));
    let attachments =
        json!([{"blob":blob,"name":"材料.txt","media_type":"text/plain","size":bytes.len()}]);
    let id = "matrix-attachments";
    let mut first = create(id, "带附件的首条");
    first.args["attachments"] = attachments.clone();
    matrix(Scenario {
        name: "create-and-send/attachments",
        auto_title: false,
        uploads: vec![bytes.to_vec()],
        script: vec![], idle_ms:3_600_000,
        steps: vec![
            Step { command:first, until:|s| status_is(s,"active") },
            Step { command:command("matrix-attached-send", "session.send", json!({"session":session_id_for(id),"text":"第二个附件","attachments":attachments})), until:|s| prompt(s,"第二个附件").is_some_and(|p| p.data["state"] == "landed") },
        ],
        check: |h,s| {
            for text in ["带附件的首条", "第二个附件"] {
                let a = &prompt(s,text).unwrap().data["attachments"][0];
                assert_eq!(a["name"], "材料.txt");
                assert_eq!(applied_counts(h).get(&format!("send:{text}")), Some(&1));
            }
            for (_, act) in h.adapter.received() {
                if let nd_backend::Act::Send { msg, .. } = act {
                    assert_eq!(msg.attachments.len(),1);
                    assert_eq!(msg.attachments[0].size,17);
                }
            }
        },
    }).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachment_creation_compensation_releases_unused_uploads_at_every_commit_point() {
    use sha2::{Digest, Sha256};
    let bytes = b"unused matrix upload";
    let blob = format!("{:x}", Sha256::digest(bytes));
    let mut first = create("matrix-attached-failure", "带附件的失败创建");
    first.args["attachments"] =
        json!([{"blob":blob,"name":"材料.txt","media_type":"text/plain","size":bytes.len()}]);
    matrix(Scenario {
        name: "create/attachments-open-fails",
        auto_title: false,
        uploads: vec![bytes.to_vec()],
        script: vec![(ActKind::Open, Reply::Fail("没有工作目录".into()))],
        idle_ms: 3_600_000,
        steps: vec![Step {
            command: first,
            until: |s| status_is(s, "withdrawn"),
        }],
        check: |h, _| {
            assert!(h.adapter.applied().is_empty());
            let blobs = nd_store::Blobs::open(h.dir.path().join("blobs"), h.store.clone()).unwrap();
            assert_eq!(blobs.collect(Duration::ZERO).unwrap(), 1);
        },
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn attached_draft_conflict_and_send_consumption_recover_at_every_commit_point() {
    use sha2::{Digest, Sha256};
    let bytes = b"matrix draft attachment";
    let attachments = json!([{"blob":format!("{:x}",Sha256::digest(bytes)),"name":"draft.txt","size":bytes.len(),"media_type":"text/plain"}]);
    let session = session_id_for("matrix-draft-create");
    let update = |id: &str, version: u64, text: &str| nd_wire::Command {
        id: id.into(),
        device: id.into(),
        name: "session.draft.update".into(),
        args: json!({"session":session,"text":text,"attachments":attachments}),
        expect: json!({"draft_version":version}),
    };
    let send = nd_wire::Command {
        id: "matrix-draft-send".into(),
        device: "a".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"胜出的草稿","attachments":attachments}),
        expect: json!({"draft_version":1}),
    };
    matrix(Scenario {
        name: "send/attached-draft-conflict-consume-retry",
        auto_title: false,
        uploads: vec![bytes.to_vec()],
        script: vec![],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-draft-create", "你好"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: update("matrix-draft-a", 0, "胜出的草稿"),
                until: |s| item(s, "draft").data["version"] == 1,
            },
            Step {
                command: update("matrix-draft-b", 0, "落败稿"),
                until: |s| {
                    item(s, "draft").data["saved"]
                        .as_array()
                        .is_some_and(|a| a.len() == 1)
                },
            },
            Step {
                command: send.clone(),
                until: |s| prompt(s, "胜出的草稿").is_some_and(|i| i.data["state"] == "landed"),
            },
            Step {
                command: update("matrix-draft-new", 2, "后来编辑"),
                until: |s| item(s, "draft").data["version"] == 3,
            },
            Step {
                command: send,
                until: |s| item(s, "draft").data["text"] == "后来编辑",
            },
        ],
        check: |h, s| {
            let d = &item(s, "draft").data;
            assert_eq!(d["attachments"][0]["name"], "draft.txt");
            assert_eq!(d["saved"][0]["attachments"][0]["name"], "draft.txt");
            assert_eq!(
                prompt(s, "胜出的草稿").unwrap().data["attachments"][0]["name"],
                "draft.txt"
            );
            assert_eq!(d["text"], "后来编辑");
            assert_eq!(d["version"], 3);
            assert_eq!(d["saved"].as_array().unwrap().len(), 1);
            assert_eq!(d["saved"][0]["text"], "落败稿");
            assert_eq!(applied_counts(h).get("send:胜出的草稿"), Some(&1));
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn withdrawal_restores_once_at_every_commit_point() {
    use sha2::{Digest, Sha256};
    let original = b"original draft attachment";
    let returned = b"returned message attachment";
    let original_ref = json!({"blob":format!("{:x}",Sha256::digest(original)),"name":"original.txt","media_type":"text/plain","size":original.len()});
    let returned_ref = json!({"blob":format!("{:x}",Sha256::digest(returned)),"name":"returned.txt","media_type":"text/plain","size":returned.len()});
    let session = session_id_for("matrix-withdraw");
    matrix(Scenario {
        auto_title: false,
        uploads: vec![original.to_vec(),returned.to_vec()],
        name: "withdraw/queued",
        idle_ms: 3_600_000,
        script: vec![(ActKind::Send, Reply::Ok), (ActKind::Send, Reply::Hold)],
        steps: vec![
            Step {
                command: create("matrix-withdraw", "ready"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: nd_wire::Command { id:"base-draft".into(),device:"test".into(),name:"session.draft.update".into(),args:json!({"session":session,"text":"existing","attachments":[original_ref]}),expect:json!({"draft_version":0}) },
                until: |s|item(s,"draft").data["version"] == 1,
            },
            Step {
                command: command(
                    "queued",
                    "session.send",
                    json!({"session":session,"text":"restore once","intent":"after_turn","attachments":[returned_ref]}),
                ),
                until: |s| prompt(s, "restore once").is_some(),
            },
            Step {
                command: command(
                    "withdraw",
                    "session.withdraw",
                    json!({"session":session,"message":"queued"}),
                ),
                until: |s| {
                    prompt(s, "restore once").is_some_and(|i| i.data["state"] == "withdrawn")
                },
            },
        ],
        check: |h, s| {
            assert_eq!(item(s, "draft").data["text"], "existing\n\nrestore once");
            assert_eq!(item(s,"draft").data["attachments"].as_array().unwrap().len(),2);
            assert_eq!(item(s, "draft").data["version"], 2);
            assert!(!h.adapter.applied().iter().any(|a| a == "send:restore once"));
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_is_accepted_during_a_wait_and_recovers_at_every_commit_point() {
    let session = session_id_for("matrix-interrupt");
    matrix(Scenario {
        auto_title: false,
        uploads: vec![],
        name: "interrupt/queued",
        idle_ms: 3_600_000,
        script: vec![(ActKind::Send, Reply::Ok), (ActKind::Send, Reply::Hold)],
        steps: vec![
            Step {
                command: create("matrix-interrupt", "ready"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "pending",
                    "session.send",
                    json!({"session":session,"text":"still queued"}),
                ),
                until: |s| prompt(s, "still queued").is_some(),
            },
            Step {
                command: command("interrupt", "session.interrupt", json!({"session":session})),
                until: |s| {
                    s.items
                        .iter()
                        .any(|i| i.id == "control/interrupt" && i.data["state"] == "acknowledged")
                },
            },
        ],
        check: |h, s| {
            assert_eq!(
                h.adapter
                    .applied()
                    .iter()
                    .filter(|a| a.starts_with("interrupt:"))
                    .count(),
                1
            );
            assert_ne!(
                prompt(s, "still queued").unwrap().data["state"],
                "withdrawn"
            );
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_the_queue_restores_once_at_every_commit_point() {
    let session = session_id_for("matrix-cancel");
    matrix(Scenario {
        auto_title: false,
        uploads: vec![],
        name: "interrupt/cancel-queue",
        idle_ms: 3_600_000,
        script: vec![(ActKind::Send, Reply::Ok), (ActKind::Send, Reply::Hold)],
        steps: vec![
            Step {
                command: create("matrix-cancel", "ready"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "pending",
                    "session.send",
                    json!({"session":session,"text":"back to draft"}),
                ),
                until: |s| prompt(s, "back to draft").is_some(),
            },
            Step {
                command: command(
                    "interrupt",
                    "session.interrupt",
                    json!({"session":session,"queued":"cancel"}),
                ),
                until: |s| {
                    prompt(s, "back to draft").is_some_and(|i| i.data["state"] == "withdrawn")
                },
            },
        ],
        check: |_, s| {
            assert_eq!(item(s, "draft").data["text"], "back to draft");
            assert_eq!(item(s, "draft").data["version"], 1);
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_and_manual_title_recover_at_every_commit_point() {
    let session = session_id_for("matrix-settings");
    matrix(Scenario {
        name: "configure-and-title/success",
        auto_title: false,
        uploads: vec![],
        script: vec![],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-settings", "hello"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "configure",
                    "session.configure",
                    json!({"session":session,"setting":{"model":"opus"}}),
                ),
                until: |s| header(s)["model"] == "opus" && header(s)["op"].is_null(),
            },
            Step {
                command: command(
                    "rename",
                    "session.rename",
                    json!({"session":session,"title":"矩阵标题"}),
                ),
                until: |s| header(s)["title"] == "矩阵标题" && header(s)["op"].is_null(),
            },
        ],
        check: |_, s| {
            assert_eq!(header(s)["model"], "opus");
            assert_eq!(header(s)["title"], "矩阵标题");
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_and_title_failures_keep_previous_values_at_every_commit_point() {
    let session = session_id_for("matrix-settings-failure");
    matrix(Scenario {
        name: "configure-and-title/refused",
        auto_title: false,
        uploads: vec![],
        script: vec![
            (ActKind::Configure, Reply::Fail("CLI 拒绝".into())),
            (ActKind::Title, Reply::Fail("标题不可用".into())),
        ],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-settings-failure", "hello"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "configure",
                    "session.configure",
                    json!({"session":session,"setting":{"model":"opus"}}),
                ),
                until: |s| {
                    s.items.iter().any(|i| {
                        i.kind == "op"
                            && i.data["kind"] == "configure"
                            && i.data["phase"] == "compensated"
                    })
                },
            },
            Step {
                command: command(
                    "rename",
                    "session.rename",
                    json!({"session":session,"title":"不应落定"}),
                ),
                until: |s| {
                    s.items.iter().any(|i| {
                        i.kind == "op"
                            && i.data["kind"] == "title"
                            && i.data["phase"] == "compensated"
                    })
                },
            },
        ],
        check: |_, s| {
            assert_eq!(header(s)["model"], "claude-haiku-4-5");
            assert_eq!(header(s)["title"], "hello");
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_automatic_title_recovers_or_keeps_summary_at_every_commit_point() {
    for fail in [false, true] {
        matrix(Scenario {
            name: if fail {
                "title/fallback"
            } else {
                "title/generate"
            },
            auto_title: true,
            uploads: vec![],
            script: if fail {
                vec![(ActKind::Title, Reply::Fail("当前版本不支持生成标题".into()))]
            } else {
                vec![]
            },
            idle_ms: 3_600_000,
            steps: vec![Step {
                command: create("matrix-title", "为当前会话自动生成可识别的标题"),
                until: |s| {
                    header(s)["status"] == "active"
                        && header(s)["op"].is_null()
                        && s.items
                            .iter()
                            .any(|i| i.kind == "op" && i.data["kind"] == "title")
                },
            }],
            check: if fail {
                |_, s| {
                    assert_eq!(header(s)["title_source"], "summary");
                    assert_eq!(header(s)["title"], "为当前会话自动生成可识别的标题");
                }
            } else {
                |_, s| {
                    assert_eq!(header(s)["title_source"], "ai");
                    assert_eq!(header(s)["title"], "生成的标题");
                }
            },
        })
        .await;
    }
}

fn invoke_state(s: &Snapshot, id: &str) -> Option<String> {
    s.items
        .iter()
        .find(|i| i.id == format!("invoke/{id}"))
        .and_then(|i| i.data["state"].as_str().map(str::to_owned))
}

fn receipt(h: &Harness, id: &str) -> nd_wire::Receipt {
    match nd_ledger::lookup(&h.store, id, None).unwrap() {
        nd_wire::ReceiptLookup::Found { receipt } => receipt,
        other => panic!("no receipt for {id}: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn summarize_from_here_backfills_the_draft_once_at_every_commit_point() {
    let session = session_id_for("matrix-sum-create");
    matrix(Scenario {
        auto_title: false,
        name: "compact/from-ok",
        uploads: vec![],
        script: vec![],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-sum-create", "第一条"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "matrix-sum-send",
                    "session.send",
                    json!({"session":session,"text":"第二条"}),
                ),
                until: |s| prompt(s, "第二条").is_some_and(|p| p.data["state"] == "landed"),
            },
            Step {
                command: nd_wire::Command {
                    id: "matrix-sum-draft".into(),
                    device: "a".into(),
                    name: "session.draft.update".into(),
                    args: json!({"session":session,"text":"旧稿"}),
                    expect: json!({"draft_version":0}),
                },
                until: |s| item(s, "draft").data["version"] == 1,
            },
            Step {
                command: command(
                    "matrix-sum",
                    "session.compact",
                    json!({"session":session,"message":"matrix-sum-send","scope":"from"}),
                ),
                until: |s| invoke_state(s, "matrix-sum").as_deref() == Some("done"),
            },
        ],
        check: |h, s| {
            assert_eq!(
                applied_counts(h).get("invoke:compact:From:第二条"),
                Some(&1)
            );
            let d = &item(s, "draft").data;
            assert_eq!(d["text"], "第二条", "{d}");
            assert_eq!(d["version"], 2, "backfilled exactly once: {d}");
            let saved = d["saved"].as_array().unwrap();
            assert_eq!(saved.len(), 1, "{d}");
            assert_eq!(saved[0]["text"], "旧稿");
            assert_eq!(
                item(s, "lineage").data["summarized"],
                json!(["matrix-sum-send"])
            );
            let nd_wire::Receipt::Done { value } = receipt(h, "matrix-sum") else {
                panic!("compact receipt is not Done");
            };
            assert_eq!(value["draft"]["text"], "第二条");
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bang_whose_result_is_lost_is_unknown_and_never_rerun_at_every_commit_point() {
    let session = session_id_for("matrix-bang-create");
    matrix(Scenario {
        auto_title: false,
        name: "shell/unknown",
        uploads: vec![],
        script: vec![(ActKind::Invoke, Reply::Unknown("mod 重载".into()))],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-bang-create", "你好"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "matrix-bang",
                    "session.shell",
                    json!({"session":session,"command":"pwd"}),
                ),
                until: |s| invoke_state(s, "matrix-bang").as_deref() == Some("unknown"),
            },
        ],
        check: |h, _| {
            assert_eq!(applied_counts(h).get("invoke:shell:pwd?"), Some(&1));
            assert!(
                matches!(receipt(h, "matrix-bang"), nd_wire::Receipt::Unknown { .. }),
                "an unknown bang keeps an Unknown receipt"
            );
        },
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subtask_dispatches_one_fork_subagent_at_every_commit_point() {
    let session = session_id_for("matrix-fork-create");
    matrix(Scenario {
        auto_title: false,
        name: "subtask/ok",
        uploads: vec![],
        script: vec![],
        idle_ms: 3_600_000,
        steps: vec![
            Step {
                command: create("matrix-fork-create", "你好"),
                until: |s| status_is(s, "active"),
            },
            Step {
                command: command(
                    "matrix-fork",
                    "session.subtask",
                    json!({"session":session,"prompt":"查资料"}),
                ),
                until: |s| invoke_state(s, "matrix-fork").as_deref() == Some("done"),
            },
        ],
        check: |h, _| {
            assert_eq!(applied_counts(h).get("invoke:fork:查资料"), Some(&1));
            let nd_wire::Receipt::Done { value } = receipt(h, "matrix-fork") else {
                panic!("subtask receipt is not Done");
            };
            assert!(
                value["agent"]
                    .as_str()
                    .is_some_and(|a| a.starts_with("agent-"))
            );
        },
    })
    .await;
}
