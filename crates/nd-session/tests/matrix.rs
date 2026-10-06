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
    script: Vec<(ActKind, Reply)>,
    idle_ms: u64,
    steps: Vec<Step>,
    check: fn(&Harness, &Snapshot),
}

fn config_for(idle_ms: u64, faults: Option<Arc<dyn Faults>>) -> EngineConfig {
    EngineConfig {
        idle_reclaim: Duration::from_millis(idle_ms),
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
        let _ = h.sessions.execute(&step.command).await;
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
            h = h.restart(config_for(scenario.idle_ms, None)).await;
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
async fn create_reaches_active_or_is_compensated_at_every_commit_point() {
    matrix(Scenario {
        name: "create/ok",
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
        uploads: vec![],
        script: vec![(ActKind::Send, Reply::Unknown("进程退出，没等到回显".into()))],
        idle_ms: 3_600_000,
        steps: vec![Step {
            command: create("matrix-create-partial", "你好"),
            until: |s| status_is(s, "partial"),
        }],
        check: |_, s| {
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
