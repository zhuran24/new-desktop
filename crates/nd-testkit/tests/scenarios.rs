#![cfg(feature = "scenarios")]
use nd_testkit::{CommandFault, ModelReply, Route, Scenario, ScenarioOptions};
use nd_wire::{Command, CommandReply, Receipt};
use serde_json::json;
use std::time::Duration;

fn options(name: &str) -> ScenarioOptions {
    ScenarioOptions::new(
        name,
        std::env::var_os("ND_TEST_DAEMON").expect("run scripts/test-scenarios.sh"),
    )
}

#[tokio::test]
async fn parallel_scenarios_keep_receipts_and_restart_faults_independent() {
    let (one, two) = tokio::join!(
        Scenario::start(options("parallel")),
        Scenario::start(options("parallel"))
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert_ne!(one.root(), two.root());
    assert!(one.units().iter().all(|u| !two.units().contains(u)));
    let mut a = one.connect().await.unwrap();
    let mut b = two.connect().await.unwrap();
    one.arm_command_fault("same-id", CommandFault::CrashAfterCommit)
        .unwrap();
    let ca = note("same-id", "one");
    let cb = note("same-id", "two");
    let (ra, rb) = tokio::join!(a.command(&ca), b.command(&cb));
    assert!(matches!(
        ra.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    assert!(matches!(
        rb.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    assert!(one.command_fault_consumed());
    let a_epoch = a.subscribe("global").await.unwrap().epoch;
    let b_epoch = b.subscribe("global").await.unwrap().epoch;
    one.kill_daemon().unwrap();
    let mut a = one.connect().await.unwrap();
    two.restart_daemon().unwrap();
    let mut b = two.connect().await.unwrap();
    assert_ne!(a.subscribe("global").await.unwrap().epoch, a_epoch);
    assert_ne!(b.subscribe("global").await.unwrap().epoch, b_epoch);
    assert_eq!(
        a.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"]["text"],
        "one"
    );
    assert_eq!(
        b.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"]["text"],
        "two"
    );
    one.close().unwrap();
    // Closing one scenario cannot affect the other.
    assert_eq!(
        b.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"]["text"],
        "two"
    );
    two.close().unwrap();
}
fn note(id: &str, text: &str) -> Command {
    Command {
        id: id.into(),
        device: "scenario".into(),
        name: "diagnostics.set_note".into(),
        args: json!({"text":text}),
        expect: json!({"revision":0}),
    }
}

#[tokio::test]
async fn sample_uses_real_daemon_and_replica_and_removes_its_resources() {
    let scenario = Scenario::start(options("sample")).await.unwrap();
    let root = scenario.root().to_owned();
    let units = scenario.units();
    let mut ui = scenario.connect().await.unwrap();
    let snapshot = ui.subscribe("global").await.unwrap();
    assert_eq!(snapshot.stream, "global");
    assert_eq!(
        ui.command(&note("sample", "fixture works")).await.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Done {
                value: json!({"revision":1})
            }
        }
    );
    drop(ui);
    scenario.close().unwrap();
    assert!(!root.exists());
    for unit in units {
        let out = std::process::Command::new("systemctl")
            .args([
                "--user",
                "list-units",
                &unit,
                "--all",
                "--plain",
                "--no-legend",
                "--no-pager",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            out.stdout.is_empty(),
            "{unit}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[tokio::test]
async fn real_process_is_isolated_and_fifo_controls_completion_in_a_limited_slice() {
    let mut scenario = Scenario::start(options("fifo")).await.unwrap();
    let mut gate = scenario.fifo("task").unwrap();
    let script = r#"
import json, os, socket
from pathlib import Path
s = socket.socket(); s.settimeout(0.3)
try:
    s.connect(('192.0.2.1', 443)); blocked = False
except OSError:
    blocked = True
print(json.dumps({'home': os.environ['HOME'], 'config':os.environ['CLAUDE_CONFIG_DIR'],
    'env_keys':sorted(os.environ), 'owner_visible':Path('/home/zhuran24').exists(),
    'blocked':blocked}), flush=True)
with open('/sandbox/fifos/task') as gate:
    print(gate.readline().strip(), flush=True)
"#;
    let process = scenario
        .spawn(
            "fifo-reader",
            nd_testkit::Program::new("/usr/bin/python3").args(["-c", script]),
        )
        .unwrap();
    process
        .wait_for_stdout("blocked", Duration::from_secs(5))
        .await
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_str(process.stdout().unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(report["home"], "/sandbox/home");
    assert_eq!(report["config"], "/sandbox/claude");
    assert_eq!(report["blocked"], true);
    assert_eq!(report["owner_visible"], false);
    for key in [
        "BUN_OPTIONS",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_SMALL_FAST_MODEL",
    ] {
        assert!(!report["env_keys"].as_array().unwrap().contains(&json!(key)));
    }
    let limits = scenario.limits().unwrap();
    assert_eq!(limits.memory_max, 2147483648);
    assert_eq!(limits.memory_swap_max, 0);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(40),
            process.wait(Duration::from_secs(5))
        )
        .await
        .is_err()
    );
    gate.release("completed").unwrap();
    assert_eq!(process.wait(Duration::from_secs(5)).await.unwrap(), 0);
    assert!(process.stdout().unwrap().ends_with("completed\n"));
    scenario.close().unwrap();
}

#[tokio::test]
async fn pinned_real_claude_receives_held_streaming_reply_without_network_or_credentials() {
    let mut scenario = Scenario::start(options("claude-text")).await.unwrap();
    let route = Route::new(None, "claude-haiku-4-5");
    let gate = scenario
        .endpoint()
        .enqueue_held(route.clone(), ModelReply::text("OFFLINE_CLAUDE_OK"));
    let cli = scenario
        .spawn(
            "claude",
            nd_testkit::Program::claude().args([
                "-p",
                "offline fixture hello",
                "--model",
                "claude-haiku-4-5",
                "--output-format",
                "stream-json",
                "--verbose",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--tools",
                "",
            ]),
        )
        .unwrap();
    scenario
        .endpoint()
        .wait_for_requests(&route, 1, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), cli.wait(Duration::from_secs(5)))
            .await
            .is_err()
    );
    gate.release();
    assert_eq!(
        cli.wait(Duration::from_secs(30)).await.unwrap(),
        0,
        "{}",
        cli.stderr().unwrap()
    );
    let frames: Vec<serde_json::Value> = cli
        .stdout()
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = frames
        .iter()
        .find(|frame| frame["type"] == "result")
        .unwrap();
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], "OFFLINE_CLAUDE_OK");
    assert_eq!(scenario.endpoint().count(&route), 1);
    assert_eq!(scenario.endpoint().requests()[0].body["stream"], true);
    scenario.close().unwrap();
}

#[tokio::test]
async fn real_claude_executes_scripted_tool_and_reports_fifo_result_to_endpoint() {
    let mut scenario = Scenario::start(options("claude-tool")).await.unwrap();
    let mut fifo = scenario.fifo("cli-task").unwrap();
    let route = Route::new(None, "claude-haiku-4-5");
    scenario.endpoint().enqueue(route.clone(), ModelReply::tool("tool_gate", "Bash", json!({
        "command": "read -r line < /sandbox/fifos/cli-task; printf 'FIFO_RESULT:%s\\n' \"$line\"",
        "description": "Wait for the offline scenario FIFO", "timeout": 30000
    })));
    scenario
        .endpoint()
        .enqueue(route.clone(), ModelReply::text("TOOL_CONFIRMED"));
    let cli = scenario
        .spawn(
            "claude",
            nd_testkit::Program::claude().args([
                "-p",
                "run offline fixture",
                "--model",
                "claude-haiku-4-5",
                "--output-format",
                "stream-json",
                "--verbose",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--tools",
                "Bash",
                "--allowedTools",
                "Bash",
            ]),
        )
        .unwrap();
    cli.wait_for_stdout("tool_gate", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(scenario.endpoint().count(&route), 1);
    fifo.release("released-by-test").unwrap();
    assert_eq!(
        cli.wait(Duration::from_secs(30)).await.unwrap(),
        0,
        "{}",
        cli.stderr().unwrap()
    );
    let requests = scenario.endpoint().requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1].body["messages"].as_array().unwrap();
    let tool_result = messages
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == "tool_gate")
        .unwrap();
    assert!(
        tool_result
            .to_string()
            .contains("FIFO_RESULT:released-by-test"),
        "{tool_result}"
    );
    assert!(cli.stdout().unwrap().contains("TOOL_CONFIRMED"));
    scenario.close().unwrap();
}

#[tokio::test]
async fn dropping_a_scenario_stops_its_whole_process_tree() {
    let mut scenario = Scenario::start(options("drop-cleanup")).await.unwrap();
    let root = scenario.root().to_owned();
    let _fifo = scenario.fifo("blocked").unwrap();
    let process = scenario
        .spawn(
            "blocked-child",
            nd_testkit::Program::new("/usr/bin/bash").args([
                "-c",
                "echo waiting; (read -r line < /sandbox/fifos/blocked) & wait",
            ]),
        )
        .unwrap();
    process
        .wait_for_stdout("waiting", Duration::from_secs(5))
        .await
        .unwrap();
    let out = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            process.unit(),
            "-p",
            "ControlGroup",
            "--value",
        ])
        .output()
        .unwrap();
    let cgroup = std::path::Path::new("/sys/fs/cgroup").join(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_start_matches('/'),
    );
    assert!(cgroup.join("cgroup.procs").exists());
    drop(scenario);
    assert!(!root.exists());
    assert!(
        !cgroup.exists(),
        "all descendants must exit before cleanup returns"
    );
}

#[tokio::test]
async fn failed_start_cleans_up_its_transient_services_and_slice() {
    let name = format!("fail-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let mut options = ScenarioOptions::new(&name, "/usr/bin/false");
    options.timeout = Duration::from_millis(250);
    assert!(Scenario::start(options).await.is_err());
    for pattern in [
        format!("nd-test-{name}-*"),
        format!("nd-test-{}*.slice", name.replace('-', "")),
    ] {
        let out = std::process::Command::new("systemctl")
            .args([
                "--user",
                "list-units",
                &pattern,
                "--all",
                "--plain",
                "--no-legend",
                "--no-pager",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            out.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
