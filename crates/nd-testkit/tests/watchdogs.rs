#![cfg(feature = "scenarios")]
use nd_testkit::{Scenario, ScenarioOptions};
use std::time::Duration;

fn options(name: &str) -> ScenarioOptions {
    let mut options = ScenarioOptions::new(name, std::env::var_os("ND_TEST_DAEMON").unwrap());
    options.watchdog = Some(std::env::var_os("ND_TEST_WATCHDOG").unwrap().into());
    options
}

async fn records_until(
    link: &mut nd_runs::WatchLink,
    after: u64,
    until: impl Fn(&[nd_watchdog_proto::Record]) -> bool,
) -> Vec<nd_watchdog_proto::Record> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = link.read(after, 1000).await.unwrap();
            if until(&rows) {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("watchdog output condition must become observable")
}
async fn input_recorded(runs: &nd_runs::Watchdogs, run: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runs
                .records(run, 0, 100)
                .unwrap()
                .iter()
                .any(|r| matches!(r.event, nd_watchdog_proto::Event::In { in_seq: 1, .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("input must reach the real pipe writer");
}

#[tokio::test]
async fn daemon_restart_preserves_backend_identity_and_numbered_io() {
    let scenario = Scenario::start(options("watchdog")).await.unwrap();
    let spec = scenario.watchdog_spec("echo", "/usr/bin/cat", &[]).unwrap();
    let runs = scenario.watchdogs().unwrap();
    let first = runs.launch("echo", spec.clone()).await.unwrap();
    assert_eq!(runs.launch("echo", spec).await.unwrap(), first);
    let mut link = runs.link("echo").await.unwrap();
    link.write(1, "hello").await.unwrap();
    link.write(1, "hello").await.unwrap();
    let before = records_until(&mut link, 0, |rows| {
        rows.iter()
            .any(|r| matches!(&r.event, nd_watchdog_proto::Event::Out { line } if line == "hello"))
    })
    .await;
    assert_eq!(
        before
            .iter()
            .filter(|r| matches!(&r.event, nd_watchdog_proto::Event::Out{line} if line == "hello"))
            .count(),
        1
    );
    drop(link);
    scenario.restart_daemon().unwrap();
    let mut ui = scenario.connect().await.unwrap();
    let page = ui.get("runs", Default::default()).await.unwrap();
    assert_eq!(
        page.items[0].data["identity"],
        serde_json::to_value(first.identity).unwrap()
    );
    let mut link = runs.link("echo").await.unwrap();
    assert_eq!(link.read(0, 100).await.unwrap(), before);
    link.write(2, "still here").await.unwrap();
    records_until(&mut link, before.last().unwrap().end_seq, |rows| {
        rows.iter().any(
            |r| matches!(&r.event, nd_watchdog_proto::Event::Out { line } if line == "still here"),
        )
    })
    .await;
    scenario.close().unwrap();
}

#[tokio::test]
async fn soft_limit_marks_only_deltas_and_hard_limit_preserves_facts_in_overflow() {
    use nd_watchdog_proto::{Event, GapReason};
    let scenario = Scenario::start(options("overflow")).await.unwrap();
    let mut spec = scenario
        .watchdog_spec("overflow", "/usr/bin/cat", &[])
        .unwrap();
    spec.limits.soft_bytes = 1;
    spec.limits.hard_bytes = 512;
    let runs = scenario.watchdogs().unwrap();
    runs.launch("overflow", spec).await.unwrap();
    let mut link = runs.link("overflow").await.unwrap();
    for i in 1..=20 {
        link.write(
            i,
            if i < 20 {
                r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#
            } else {
                r#"{"type":"result","result":"complete"}"#
            },
        )
        .await
        .unwrap();
    }
    let rows = records_until(&mut link, 0, |rows| {
        rows.iter()
            .any(|r| matches!(&r.event, Event::Out { line } if line.contains("complete")))
    })
    .await;
    assert!(rows.iter().any(|r| matches!(
        r.event,
        Event::Gap {
            reason: GapReason::DeltaDropped
        }
    )));
    assert!(
        !rows
            .iter()
            .any(|r| matches!(&r.event, Event::Out { line } if line.contains("stream_event")))
    );
    assert!(
        rows.iter()
            .any(|r| matches!(&r.event, Event::Out { line } if line.contains("complete")))
    );
    assert!(!rows.iter().any(|r| matches!(
        r.event,
        Event::Gap {
            reason: GapReason::LostLines
        }
    )));
    assert!(link.stats().await.unwrap().overflow_bytes > 0);
    link.ack(rows.last().unwrap().end_seq).await.unwrap();
    assert!(
        link.read(rows.last().unwrap().end_seq, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(link.stats().await.unwrap().retained_bytes < 512);
    scenario.close().unwrap();
}

#[tokio::test]
async fn storage_failure_reports_lost_lines_and_never_claims_a_clean_tail() {
    use nd_watchdog_proto::{Event, GapReason};
    let scenario = Scenario::start(options("lost-lines")).await.unwrap();
    let mut spec = scenario.watchdog_spec("lost", "/usr/bin/python3", &["-u", "-c", "import time\nfor i in range(200):\n print('fact-' + str(i)); time.sleep(0.005)\ntime.sleep(20)"]).unwrap();
    spec.limits.soft_bytes = 0;
    spec.limits.hard_bytes = 0;
    let overflow = scenario.root().join("cannot-be-a-directory");
    std::fs::write(&overflow, "occupied").unwrap();
    spec.limits.overflow = overflow;
    let runs = scenario.watchdogs().unwrap();
    runs.launch("lost", spec).await.unwrap();
    let mut link = runs.link("lost").await.unwrap();
    let rows = records_until(&mut link, 0, |rows| {
        rows.iter().any(|r| {
            matches!(
                r.event,
                Event::Gap {
                    reason: GapReason::LostLines
                }
            )
        })
    })
    .await;
    assert!(rows.iter().any(|r| matches!(
        r.event,
        Event::Gap {
            reason: GapReason::LostLines
        }
    )));
    assert!(link.stats().await.unwrap().lost_lines);
    let mut ui = scenario.connect().await.unwrap();
    let page = ui.get("runs", Default::default()).await.unwrap();
    assert_eq!(page.items[0].data["tail"], "Unknown");
    scenario.close().unwrap();
}

#[tokio::test]
async fn backend_exit_cleans_descendants_even_without_daemon_and_watchdog_never_restarts() {
    let scenario = Scenario::start(options("exit-cleanup")).await.unwrap();
    let script = "import subprocess,sys,os\np=subprocess.Popen(['sleep','60'])\nprint(p.pid,flush=True)\nsys.stdin.readline()\nos._exit(7)";
    let mut spec = scenario
        .watchdog_spec("exit", "/usr/bin/python3", &["-u", "-c", script])
        .unwrap();
    spec.limits.soft_bytes = 0;
    spec.limits.hard_bytes = 0;
    let overflow = spec.limits.overflow.clone();
    let runs = scenario.watchdogs().unwrap();
    let launch = runs.launch("exit", spec.clone()).await.unwrap();
    let mut link = runs.link("exit").await.unwrap();
    let rows = records_until(&mut link, 0, |rows| {
        rows.iter()
            .any(|r| matches!(r.event, nd_watchdog_proto::Event::Out { .. }))
    })
    .await;
    let descendant = rows
        .iter()
        .find_map(|r| match &r.event {
            nd_watchdog_proto::Event::Out { line } => {
                Some(nd_watchdog_proto::Identity::read(line.parse().unwrap()).unwrap())
            }
            _ => None,
        })
        .unwrap();
    systemctl(&["stop", &scenario.units()[0]]);
    link.write(1, "exit").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while launch.identity.alive() || descendant.alive() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(runs.launch("exit", spec).await.is_err());
    let found = runs.recover().await.unwrap();
    assert!(found[0].state.is_gone());
    assert_eq!(found[0].exit, Some(7));
    assert!(
        runs.records("exit", 0, 100)
            .unwrap()
            .iter()
            .any(|r| matches!(r.event, nd_watchdog_proto::Event::Exit { code: 7 }))
    );
    assert_eq!(
        runs.collect_unused(&std::collections::BTreeSet::from(["exit".to_owned()]))
            .unwrap(),
        0,
        "a committed reference keeps the unread tail"
    );
    assert!(runs.directory("exit").unwrap().exists());
    assert_eq!(runs.collect_unused(&Default::default()).unwrap(), 1);
    assert!(!runs.directory("exit").unwrap().exists());
    assert!(!overflow.exists());
    assert_eq!(
        runs.inspect().unwrap()[0].exit,
        Some(7),
        "compact tombstone preserves the exit"
    );
    let spec = scenario.watchdog_spec("exit", "/usr/bin/cat", &[]).unwrap();
    assert!(
        runs.launch("exit", spec).await.is_err(),
        "collection must not allow launch replay"
    );
    scenario.close().unwrap();
}
fn systemctl(args: &[&str]) -> String {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

#[tokio::test]
async fn n7_thousand_lines_per_second_survive_fifty_kills_and_fifty_restarts() {
    use nd_watchdog_proto::Event;
    let scenario = Scenario::start(options("n7")).await.unwrap();
    let script = "import sys,time\nstart=time.monotonic()\nn=0\nwhile True:\n for _ in range(100):\n  print(n);n+=1\n sys.stdout.flush()\n time.sleep(max(0,start+n/1000-time.monotonic()))";
    let spec = scenario
        .watchdog_spec("rate", "/usr/bin/python3", &["-u", "-c", script])
        .unwrap();
    let runs = scenario.watchdogs().unwrap();
    let launch = runs.launch("rate", spec).await.unwrap();
    let cg = systemctl(&["show", &launch.unit, "-p", "ControlGroup", "--value"]);
    let daemon_cg = systemctl(&[
        "show",
        &scenario.units()[0],
        "-p",
        "ControlGroup",
        "--value",
    ]);
    assert_ne!(cg, daemon_cg);
    assert_eq!(
        cg.rsplit_once('/').unwrap().0,
        daemon_cg.rsplit_once('/').unwrap().0
    );
    let actual = std::fs::read_to_string(format!("/proc/{}/cgroup", launch.identity.pid)).unwrap();
    assert!(actual.contains(&cg));
    assert_eq!(
        systemctl(&["show", &launch.unit, "-p", "Restart", "--value"]),
        "no"
    );
    assert!(
        systemctl(&[
            "show",
            &launch.unit,
            "-p",
            "PartOf",
            "-p",
            "BindsTo",
            "--value"
        ])
        .is_empty()
    );
    assert_eq!(
        scenario.limits().unwrap().memory_max,
        2 * 1024 * 1024 * 1024
    );
    assert_eq!(scenario.limits().unwrap().memory_swap_max, 0);
    let mut next = 0u64;
    let mut cursor = 0;
    for n in 0..100 {
        let mut ui = scenario.connect().await.unwrap();
        let old = ui.subscribe("global").await.unwrap().epoch;
        drop(ui);
        if n < 50 {
            scenario.kill_daemon().unwrap();
        } else {
            scenario.restart_daemon().unwrap();
        }
        let mut ui = scenario.connect().await.unwrap();
        assert_ne!(ui.subscribe("global").await.unwrap().epoch, old);
        let page = ui.get("runs", Default::default()).await.unwrap();
        assert_eq!(
            page.items[0].data["identity"],
            serde_json::to_value(&launch.identity).unwrap()
        );
        assert_eq!(
            page.items[0].data["state"], "Up",
            "{:?}",
            page.items[0].data
        );
        let mut link = runs.link("rate").await.unwrap();
        assert_eq!(link.hello.identity, launch.identity);
        let high = link.hello.high;
        while cursor < high {
            let rows = link.read(cursor, 1000).await.unwrap();
            assert!(!rows.is_empty());
            for row in rows {
                assert_eq!(row.seq, cursor + 1);
                assert_eq!(row.end_seq, row.seq);
                let Event::Out { line } = row.event else {
                    panic!("unexpected frame {:?}", row.event);
                };
                assert_eq!(line, next.to_string());
                next += 1;
                cursor = row.end_seq;
            }
        }
        link.release().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(next >= 10000, "producer observed only {next} lines");
    eprintln!(
        "N7: 50 SIGKILL + 50 restart; identity={:?}; verified={next}; cgroup={cg}",
        launch.identity
    );
    scenario.close().unwrap();
}

#[tokio::test]
async fn v6_real_workflow_records_versioned_fixture_and_measures_stream_share() {
    use nd_testkit::{ModelReply, Route};
    use nd_watchdog_proto::{Event, FixtureMeta};
    let scenario = Scenario::start(options("v6-workflow")).await.unwrap();
    let main = Route::new(None, "claude-haiku-4-5");
    std::fs::write(
        scenario.root().join("project/volume-input.txt"),
        "VOLUME_TOOL_OBSERVED\n".repeat(128),
    )
    .unwrap();
    let script = "export const meta = {name:'watchdog-volume',description:'Offline sequential workflow volume probe'}; for (let i=0;i<6;i++) { await agent('offline volume sample '+i, {model:'sonnet',label:'volume-'+i}); } return {completed:6};";
    scenario.endpoint().enqueue(
        main.clone(),
        ModelReply::tool(
            "workflow_volume",
            "Workflow",
            serde_json::json!({"script":script}),
        ),
    );
    for _ in 0..4 {
        scenario.endpoint().enqueue(
            main.clone(),
            ModelReply::streaming_text("WORKFLOW_OBSERVED\n".repeat(1024), 4, 2),
        );
    }
    for i in 0..6 {
        scenario.endpoint().enqueue_any_agent(
            "claude-sonnet-5-5",
            ModelReply::tool(
                &format!("volume_read_{i}"),
                "Read",
                serde_json::json!({"file_path":"/sandbox/project/volume-input.txt"}),
            ),
        );
        scenario.endpoint().enqueue_any_agent(
            "claude-sonnet-5-5",
            ModelReply::streaming_text("0123456789abcdef".repeat(4096), 16, 5),
        );
    }
    let spec = scenario.claude_watchdog_spec("workflow").unwrap();
    let runs = scenario.watchdogs().unwrap();
    runs.launch("workflow", spec).await.unwrap();
    let mut link = runs.link("workflow").await.unwrap();
    link.write(1,r#"{"type":"control_request","request_id":"init","request":{"subtype":"initialize","perTaskStopAffordance":true,"forwardSubagentText":true}}"#).await.unwrap();
    link.write(2,r#"{"type":"user","message":{"role":"user","content":"Run the offline volume workflow"},"parent_tool_use_id":null,"session_id":""}"#).await.unwrap();
    let began = std::time::Instant::now();
    let mut cursor = 0;
    let mut recorded = vec![];
    let mut completed = false;
    while began.elapsed() < Duration::from_secs(240) {
        let rows = link.read(cursor, 1000).await.unwrap();
        for row in &rows {
            if let Event::Out { line } = &row.event
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                && v["type"] == "system"
                && v["subtype"] == "task_notification"
                && v["status"] == "completed"
            {
                completed = true;
            }
            cursor = row.end_seq;
        }
        recorded.extend(rows);
        if completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        completed,
        "workflow did not complete; requests={:?}; last={:?}",
        scenario
            .endpoint()
            .requests()
            .iter()
            .map(|r| &r.route)
            .collect::<Vec<_>>(),
        recorded.iter().rev().take(8).collect::<Vec<_>>()
    );
    let stats = link.stats().await.unwrap();
    eprintln!("V6 measured elapsed={:?}; stats={stats:?}", began.elapsed());
    let main_deltas = recorded
        .iter()
        .filter_map(|row| match &row.event {
            Event::Out { line } => serde_json::from_str::<serde_json::Value>(line).ok(),
            _ => None,
        })
        .filter(|v| {
            v["type"] == "stream_event"
                && v["parent_tool_use_id"].is_null()
                && v["event"]["type"] == "content_block_delta"
                && v["event"]["delta"]["type"] == "text_delta"
        })
        .count();
    assert!(
        main_deltas >= 1000,
        "volume sample must include sustained main-conversation streaming, got {main_deltas} deltas"
    );
    let requests = scenario.endpoint().requests();
    let agents = requests
        .iter()
        .filter_map(|r| r.route.agent.as_ref())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(agents.len(), 6);
    let tool_results = requests
        .iter()
        .filter(|r| {
            r.route.agent.is_some()
                && r.body["messages"]
                    .to_string()
                    .contains("VOLUME_TOOL_OBSERVED")
        })
        .count();
    assert_eq!(
        tool_results, 6,
        "all Workflow agents must read the actual file"
    );
    assert!(stats.stdout_lines > 0);
    assert!(stats.stream_lines > 0);
    assert!(!stats.lost_lines);
    let path = scenario.root().join("recording.jsonl");
    nd_watchdog_proto::write_fixture(
        &path,
        &FixtureMeta {
            format: 1,
            capability: "watchdog".into(),
            backend: "claude".into(),
            version: "2.1.289".into(),
            scenario: "long-workflow".into(),
        },
        &recorded,
    )
    .unwrap();
    let (meta, replayed) = nd_watchdog_proto::read_fixture(&path).unwrap();
    assert_eq!(meta.scenario, "long-workflow");
    assert_eq!(replayed, recorded);
    eprintln!("V6 elapsed={:?}; stats={stats:?}", began.elapsed());
    if let Some(dest) = std::env::var_os("ND_TEST_EVIDENCE") {
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(&path, std::path::Path::new(&dest).join("v6-workflow.jsonl")).unwrap();
        std::fs::write(
            std::path::Path::new(&dest).join("v6-stats.json"),
            serde_json::to_vec_pretty(&stats).unwrap(),
        )
        .unwrap();
        std::fs::write(
            std::path::Path::new(&dest).join("v6-measurement.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "kind": "offline_streaming_sample_not_capacity_validation",
                "elapsed_seconds": began.elapsed().as_secs_f64(),
                "main_text_deltas": main_deltas,
                "workflow_agents": agents.len(),
                "observed_tool_results": tool_results,
                "main_chunk_chars": 4,
                "main_pause_ms": 2,
                "agent_chunk_chars": 16,
                "agent_pause_ms": 5,
                "stats": stats,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    scenario.close().unwrap();
}

#[tokio::test]
async fn live_process_without_its_expected_unit_is_identity_mismatch_not_gone() {
    let scenario = Scenario::start(options("identity")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let launched = runs
        .launch(
            "live",
            scenario.watchdog_spec("live", "/usr/bin/cat", &[]).unwrap(),
        )
        .await
        .unwrap();
    let mut wrong = scenario.watchdog_config().unwrap();
    wrong.unit_prefix.push_str("-absent");
    let wrong = nd_runs::Watchdogs::new(wrong).unwrap();
    assert!(wrong.link("live").await.is_err());
    assert_eq!(
        wrong.recover().await.unwrap()[0].state,
        nd_runs::RunState::IdentityMismatch
    );
    assert!(launched.identity.alive());
    scenario.close().unwrap();
}

#[tokio::test]
async fn new_controller_can_end_a_backend_while_old_input_pipe_is_blocked() {
    let scenario = Scenario::start(options("blocked-input")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let launch = runs
        .launch(
            "blocked",
            scenario
                .watchdog_spec(
                    "blocked",
                    "/usr/bin/python3",
                    &["-c", "import time; time.sleep(60)"],
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let mut old = runs.link("blocked").await.unwrap();
    let writing = tokio::spawn(async move { old.write(1, &"x".repeat(1024 * 1024)).await });
    input_recorded(&runs, "blocked").await;
    let mut current = runs.link("blocked").await.unwrap();
    tokio::time::timeout(
        Duration::from_millis(500),
        current.finish(nd_watchdog_proto::Finish::Kill),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while launch.identity.alive() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(writing.await.unwrap().is_err());
    scenario.close().unwrap();
}

#[tokio::test]
#[ignore = "会触发 KDE Memory Shortage Avoided 桌面通知，只手动跑"]
async fn manual_oom_kill_triggers_desktop_notification_only_run_manually() {
    let mut limited = options("n7-memory");
    limited.memory_max = 128 * 1024 * 1024;
    let scenario = Scenario::start(limited).await.unwrap();
    let other = Scenario::start(options("n7-unaffected")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let spec=scenario.watchdog_spec("allocate","/usr/bin/python3",&["-c","import sys,time; sys.stdin.readline(); x=bytearray(512*1024*1024)\nfor i in range(0,len(x),4096): x[i]=1\ntime.sleep(30)"]).unwrap();
    let launch = runs.launch("allocate", spec).await.unwrap();
    let slice = scenario.units().last().unwrap().clone();
    let cg = systemctl(&["show", &slice, "-p", "ControlGroup", "--value"]);
    let events = std::path::Path::new("/sys/fs/cgroup")
        .join(cg.trim_start_matches('/'))
        .join("memory.events");
    let mut link = runs.link("allocate").await.unwrap();
    link.write(1, "allocate").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while launch.identity.alive() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let events = std::fs::read_to_string(events).unwrap();
    let killed = events
        .lines()
        .find_map(|l| l.strip_prefix("oom_kill "))
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(killed >= 1, "{events}");
    let mut ui = other.connect().await.unwrap();
    assert!(!ui.subscribe("global").await.unwrap().items.is_empty());
    eprintln!("N7 memory.max=134217728; memory.events={events:?}");
    scenario.close().unwrap();
    other.close().unwrap();
}

#[tokio::test]
async fn watchdog_death_cleans_backend_and_cannot_replay_the_launch() {
    let scenario = Scenario::start(options("watchdog-death")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let spec = scenario.watchdog_spec("dead", "/usr/bin/cat", &[]).unwrap();
    let launch = runs.launch("dead", spec.clone()).await.unwrap();
    let link = runs.link("dead").await.unwrap();
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(link.hello.watchdog.pid as i32).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while launch.identity.alive() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(runs.launch("dead", spec).await.is_err());
    assert!(runs.recover().await.unwrap()[0].state.is_gone());
    scenario.close().unwrap();
}

#[tokio::test]
async fn cancelled_input_transfer_finishes_once_and_reconnect_deduplicates_its_sequence() {
    let scenario = Scenario::start(options("input-once")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let spec=scenario.watchdog_spec("once","/usr/bin/python3",&["-u","-c","import time,sys,pathlib\nwhile not pathlib.Path('/sandbox/read-once').exists(): time.sleep(.01)\nfor line in sys.stdin: print(len(line.strip()),flush=True)"]).unwrap();
    runs.launch("once", spec).await.unwrap();
    let mut old = runs.link("once").await.unwrap();
    let writing = tokio::spawn(async move { old.write(1, &"x".repeat(256 * 1024)).await });
    input_recorded(&runs, "once").await;
    let mut current = runs.link("once").await.unwrap();
    std::fs::write(scenario.root().join("read-once"), "").unwrap();
    current.write(1, &"x".repeat(256 * 1024)).await.unwrap();
    current.write(2, "marker").await.unwrap();
    let output = records_until(&mut current, 0, |rows| {
        rows.iter()
            .filter(|r| matches!(r.event, nd_watchdog_proto::Event::Out { .. }))
            .count()
            >= 2
    })
    .await
    .into_iter()
    .filter_map(|r| match r.event {
        nd_watchdog_proto::Event::Out { line } => Some(line),
        _ => None,
    })
    .collect::<Vec<_>>();
    assert_eq!(output, vec!["262144", "6"]);
    let _ = writing.await;
    scenario.close().unwrap();
}

#[tokio::test]
async fn concurrent_idempotent_launch_does_not_replace_the_adapter_controller() {
    let scenario = Scenario::start(options("launch-once")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let spec = scenario.watchdog_spec("same", "/usr/bin/cat", &[]).unwrap();
    let (a, b) = tokio::join!(
        runs.launch("same", spec.clone()),
        runs.launch("same", spec.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let mut link = runs.link("same").await.unwrap();
    runs.launch("same", spec).await.unwrap();
    link.write(1, "controller-still-owned").await.unwrap();
    scenario.close().unwrap();
}

#[tokio::test]
async fn failed_backend_spawn_reports_never_launched_with_no_live_identity() {
    let scenario = Scenario::start(options("never-launched")).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    let spec = scenario
        .watchdog_spec("missing", "/no-such-backend", &[])
        .unwrap();
    assert!(runs.launch("missing", spec).await.is_err());
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = runs.recover().await.unwrap();
            if found[0].state.is_gone() {
                break found;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(found[0].state.is_gone());
    assert_eq!(
        found[0].state,
        nd_runs::RunState::Gone {
            reason: nd_runs::GoneReason::NeverLaunched
        }
    );
    assert!(found[0].identity.is_none());
    scenario.close().unwrap();
}

#[tokio::test]
async fn n7_disk_page_cache_reaches_slice_limit_without_any_oom_kill() {
    let mut config = options("n7-page-cache");
    config.memory_max = 128 * 1024 * 1024;
    config.disk_scratch = true;
    let scenario = Scenario::start(config).await.unwrap();
    let runs = scenario.watchdogs().unwrap();
    // Flush in small batches: reclaimable file pages create pressure, anonymous RSS stays small.
    let script = "import os,sys\nblock=b'x'*(128*1024)\nwith open('/scratch/page-cache.bin','wb',buffering=0) as f:\n for batch in range(64):\n  for _ in range(64): f.write(block)\n  os.fsync(f.fileno())\nprint('cache-ready',flush=True)\nsys.stdin.readline()";
    let launch = runs
        .launch(
            "cache",
            scenario
                .watchdog_spec("cache", "/usr/bin/python3", &["-u", "-c", script])
                .unwrap(),
        )
        .await
        .unwrap();
    let mut link = runs.link("cache").await.unwrap();
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let rows = link.read(0, 10).await.unwrap();
            if rows.iter().any(
                |r| matches!(&r.event,nd_watchdog_proto::Event::Out{line} if line=="cache-ready"),
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let cg = systemctl(&[
        "show",
        scenario.units().last().unwrap(),
        "-p",
        "ControlGroup",
        "--value",
    ]);
    let cg = std::path::Path::new("/sys/fs/cgroup").join(cg.trim_start_matches('/'));
    let events = std::fs::read_to_string(cg.join("memory.events")).unwrap();
    let count = |name: &str| {
        events
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name} ")))
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    assert!(count("max") > 0, "{events}");
    assert_eq!(count("oom"), 0, "{events}");
    assert_eq!(count("oom_kill"), 0, "{events}");
    let max = std::fs::read_to_string(cg.join("memory.max"))
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    let current = std::fs::read_to_string(cg.join("memory.current"))
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    assert!(current <= max, "current={current} max={max}");
    assert_eq!(
        std::fs::metadata(scenario.disk_root().unwrap().join("page-cache.bin"))
            .unwrap()
            .len(),
        512 * 1024 * 1024
    );
    assert!(launch.identity.alive());
    eprintln!(
        "N7 page-cache: bytes=536870912 memory.current={current} memory.max={max} events={events:?}"
    );
    let disk = scenario.disk_root().unwrap().to_owned();
    scenario.close().unwrap();
    assert!(!disk.exists());
}

#[tokio::test]
async fn reconnect_allocates_after_an_input_that_is_still_blocked_in_the_pipe() {
    use nd_watchdog_proto::Event;
    let scenario = Scenario::start(options("accepted-input")).await.unwrap();
    let script = "import pathlib,time,sys\nwhile not pathlib.Path('/sandbox/read-now').exists(): time.sleep(.01)\nfor line in sys.stdin:\n print(line[0]+':'+str(len(line.strip())),flush=True)";
    let spec = scenario
        .watchdog_spec("blocked", "/usr/bin/python3", &["-u", "-c", script])
        .unwrap();
    let runs = scenario.watchdogs().unwrap();
    runs.launch("blocked", spec).await.unwrap();
    let mut old = runs.link("blocked").await.unwrap();
    let first = tokio::spawn(async move { old.write(1, &"A".repeat(200_000)).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runs
                .records("blocked", 0, 100)
                .unwrap()
                .iter()
                .any(|r| matches!(r.event, Event::In { in_seq: 1, .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut new = runs.link("blocked").await.unwrap();
    assert_eq!(new.hello.written, 0, "first write is blocked");
    let accepted = serde_json::to_value(&new.hello).unwrap()["accepted"]
        .as_u64()
        .unwrap_or(new.hello.written);
    assert_eq!(
        accepted, 1,
        "reconnected controller must reserve the in-flight input sequence"
    );
    std::fs::write(scenario.root().join("read-now"), "").unwrap();
    new.write(accepted + 1, "B").await.unwrap();
    let _ = first.await.unwrap();
    let rows = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = new.read(0, 100).await.unwrap();
            if rows
                .iter()
                .filter(|r| matches!(r.event, Event::Out { .. }))
                .count()
                == 2
            {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let output: Vec<_> = rows
        .iter()
        .filter_map(|r| match &r.event {
            Event::Out { line } => Some(line.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(output, ["A:200000", "B:1"]);
    scenario.close().unwrap();
}

#[tokio::test]
async fn a_failed_gap_extension_preserves_the_entire_previous_gap_range() {
    use nd_watchdog_proto::{Event, GapReason};
    let scenario = Scenario::start(options("gap-write-failure")).await.unwrap();
    let mut config = scenario.watchdog_config().unwrap();
    // 只对本场景看守忽略 SIGXFSZ，让真实文件限额失败返回 EFBIG；不替换 Journal。
    config.launcher.extend(["/usr/bin/python3", "-c", "import os,signal,sys; signal.signal(signal.SIGXFSZ,signal.SIG_IGN); os.execv(sys.argv[1],sys.argv[1:])"].map(str::to_owned));
    let runs = nd_runs::Watchdogs::new(config).unwrap();
    let script = "import pathlib,time\nfor _ in range(9): print('{\"type\":\"stream_event\"}',flush=True)\nwhile not pathlib.Path('/sandbox/extend-gap').exists(): time.sleep(.01)\nprint('{\"type\":\"stream_event\"}',flush=True)\nwhile not pathlib.Path('/sandbox/recover-gap').exists(): time.sleep(.01)\nprint('recovered',flush=True)\ntime.sleep(60)";
    let mut spec = scenario
        .watchdog_spec("gap", "/usr/bin/python3", &["-u", "-c", script])
        .unwrap();
    spec.limits.soft_bytes = 0;
    runs.launch("gap", spec).await.unwrap();
    let mut link = runs.link("gap").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if link
                .read(0, 100)
                .await
                .unwrap()
                .last()
                .is_some_and(|r| r.end_seq == 9)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let path = std::fs::read_dir(runs.directory("gap").unwrap().join("spool"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let size = path.metadata().unwrap().len();
    let limit = std::process::Command::new("prlimit")
        .args([
            "--pid",
            &link.hello.watchdog.pid.to_string(),
            &format!("--fsize={size}:unlimited"),
        ])
        .output()
        .unwrap();
    assert!(
        limit.status.success(),
        "{}",
        String::from_utf8_lossy(&limit.stderr)
    );
    std::fs::write(scenario.root().join("extend-gap"), "").unwrap();
    let rows = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = link.read(0, 100).await.unwrap();
            if rows.last().is_some_and(|r| r.end_seq == 10) {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rows.first().unwrap().seq,
        1,
        "failed rewrite must not erase earlier gap coverage: {rows:?}"
    );
    assert!(matches!(
        rows.last().unwrap().event,
        Event::Gap {
            reason: GapReason::LostLines
        }
    ));
    let restored = std::process::Command::new("prlimit")
        .args([
            "--pid",
            &link.hello.watchdog.pid.to_string(),
            "--fsize=unlimited:unlimited",
        ])
        .status()
        .unwrap();
    assert!(restored.success());
    std::fs::write(scenario.root().join("recover-gap"), "").unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = link.read(0, 100).await.unwrap();
            if rows.last().is_some_and(|r| r.end_seq == 11) {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!((recovered[0].seq, recovered[0].end_seq), (1, 10));
    assert!(matches!(&recovered[1].event, Event::Out { line } if line == "recovered"));
    scenario.close().unwrap();
}
