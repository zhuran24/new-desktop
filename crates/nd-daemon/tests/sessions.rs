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

#[tokio::test]
async fn an_unavailable_command_does_not_block_other_desktop_commands_or_retry_forever() {
    let fx = Fixture::start("command-unavailable", 3_600_000).await;
    let client = nd_ui_core::CommandClient::start(fx.socket()).unwrap();
    let unavailable = Command {
        id: "unsupported-command".into(),
        device: "desktop".into(),
        name: "session.not_implemented".into(),
        args: json!({"session":"missing"}),
        expect: json!({}),
    };
    let blocked_client = client.clone();
    let blocked = tokio::spawn(async move { blocked_client.command(unavailable).await });
    // A real read on a separate connection confirms the daemon is processing work.
    fx.ui().await.subscribe("global").await.unwrap();
    let accepted = tokio::time::timeout(
        Duration::from_secs(2),
        client.command(Command {
            id: "independent-command".into(),
            device: "desktop".into(),
            name: "diagnostics.set_note".into(),
            args: json!({"text":"still responsive"}),
            expect: json!({"revision":0}),
        }),
    )
    .await
    .expect("one retrying command must not monopolize the command client")
    .unwrap();
    assert!(matches!(
        accepted,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    let reply = tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .expect("persistent unavailability must be returned to the caller")
        .unwrap()
        .unwrap();
    assert!(matches!(reply, CommandReply::Unavailable { .. }));
    fx.close();
}

#[tokio::test]
async fn backend_exit_ends_the_running_round_before_the_next_prompt_resumes() {
    let fx = Fixture::start("round-backend-exit", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("round-exit", "/sandbox/project", "first").await;
    fx.wait(&session, "ready", |s| texts(s) == ["ready"]).await;
    fx.scenario.endpoint().enqueue(
        fx.main(),
        ModelReply::streaming_text(&"unfinished".repeat(100), 1, 30),
    );
    fx.send("interrupted-round", &session, "second").await;
    fx.scenario
        .endpoint()
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(20))
        .await
        .unwrap();
    let running = fx
        .wait(&session, "running round", |s| {
            lineage(s)["rounds"]
                .as_array()
                .is_some_and(|r| r.len() == 2)
        })
        .await;
    let pid = cli_pid(&fx, &running).await;
    signal(pid, rustix::process::Signal::KILL);
    let exited = fx
        .wait(&session, "backend exit observed", |s| {
            header(s)["process"]["alive"] == false
        })
        .await;
    assert_eq!(
        lineage(&exited)["rounds"][1]["complete"],
        true,
        "a dead process cannot keep its round running"
    );
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("resumed"));
    fx.send("next-round", &session, "third").await;
    let resumed = fx
        .wait(&session, "resumed", |s| {
            texts(s).contains(&"resumed".into()) && header(s)["process"]["turn_running"] == false
        })
        .await;
    assert_eq!(lineage(&resumed)["rounds"].as_array().unwrap().len(), 3);
    assert!(
        lineage(&resumed)["rounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["complete"] == true)
    );
    fx.close();
}

#[tokio::test]
async fn temporarily_unreachable_watchdog_during_recovery_keeps_the_backend_alive() {
    let fx = Fixture::start("recover-watchdog-link", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("link-recover", "/sandbox/project", "first").await;
    let before = fx.wait(&session, "ready", |s| texts(s) == ["ready"]).await;
    let pid = cli_pid(&fx, &before).await;
    let run = header(&before)["process"]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = fx
        .scenario
        .watchdogs()
        .unwrap()
        .directory(&run)
        .unwrap()
        .join("watchdog.sock");
    let parked = path.with_extension("parked");
    std::fs::rename(&path, &parked).unwrap();
    fx.scenario.restart_daemon().unwrap();
    let during = fx.peek(&session).await;
    assert_ne!(before.epoch, during.epoch);
    std::fs::rename(parked, path).unwrap();
    let recovered = fx
        .wait(&session, "original backend recovered", |s| {
            header(s)["recovering"] == false && header(s)["process"]["alive"] == true
        })
        .await;
    assert_eq!(cli_pid(&fx, &recovered).await, pid);
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("still here"));
    fx.send("after-relink", &session, "continue").await;
    let after = fx
        .wait(&session, "continued", |s| {
            texts(s) == ["ready", "still here"]
        })
        .await;
    assert_eq!(cli_pid(&fx, &after).await, pid);
    let config_path = fx.scenario.root().join("config.toml");
    let mut config: toml::Value =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    config["watchdogs"]["memory_max"] = toml::Value::Integer(3 * 1024 * 1024 * 1024);
    std::fs::write(config_path, toml::to_string(&config).unwrap()).unwrap();
    let mut ui = fx.ui().await;
    let mut global = ui.subscribe("global").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !global.items.iter().any(|i| {
            i.data["config_error"]
                .as_str()
                .is_some_and(|s| s.contains("须重启"))
        }) {
            global = ui.next().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(cli_pid(&fx, &fx.peek(&session).await).await, pid);
    fx.close();
}

#[tokio::test]
async fn an_uncertain_unwritten_interrupt_is_clarified_without_executing_it() {
    let fx = Fixture::start("nd18-unknown-control", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("unknown-control", "/sandbox/project", "first")
        .await;
    fx.wait(&session, "active", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("not interrupted"));
    fx.send("held", &session, "active").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    std::fs::write(
        fx.scenario.root().join("runtime/delivery-fault.json"),
        r#"{"contains":"interrupt","action":"unknown_without_write"}"#,
    )
    .unwrap();
    fx.command(
        "uncertain-esc",
        "session.interrupt",
        json!({"session":session}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = fx.peek(&session).await;
            if s.items
                .iter()
                .any(|i| i.id == "control/uncertain-esc" && i.data["state"] == "unknown")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("control unknown fault must be observed");
    fx.scenario.restart_daemon().unwrap();
    let clarified = fx
        .wait(&session, "known unwritten control", |s| {
            s.items
                .iter()
                .any(|i| i.id == "control/uncertain-esc" && i.data["state"] == "failed")
        })
        .await;
    assert_eq!(
        header(&clarified)["process"]["turn_running"],
        true,
        "Unknown controls are reconciled, not re-executed"
    );
    gate.release();
    fx.wait(&session, "original reply", |s| {
        texts(s).contains(&"not interrupted".into())
    })
    .await;
    assert_eq!(endpoint.count(&fx.main()), 2);
    fx.close();
}

#[tokio::test]
async fn a_recovered_source_accepts_escape_while_another_session_is_still_recovering() {
    let fx = Fixture::start("nd18-control-recovery", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready a"));
    let a = fx.create("source-a", "/sandbox/project", "first").await;
    fx.wait(&a, "active a", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    endpoint.enqueue(fx.main(), ModelReply::text("ready b"));
    let b = fx.create("source-b", "/sandbox/project", "second").await;
    let before = fx
        .wait(&b, "active b", |s| {
            has_header(s, |h| h["status"] == "active")
        })
        .await;
    let blocked_pid = cli_pid(&fx, &before).await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("must not finish"));
    fx.send("active-a", &a, "hold a").await;
    endpoint
        .wait_for_requests(&fx.main(), 3, Duration::from_secs(30))
        .await
        .unwrap();
    signal(blocked_pid, rustix::process::Signal::STOP);
    fx.scenario.restart_daemon().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        fx.command("esc-a", "session.interrupt", json!({"session":a})),
    )
    .await;
    let still_recovering = header(&fx.peek(&a).await)["recovering"] == true;
    signal(blocked_pid, rustix::process::Signal::CONT);
    assert!(
        matches!(
            result,
            Ok(CommandReply::Receipt {
                receipt: Receipt::Done { .. }
            })
        ),
        "Esc must use source readiness, not the global write gate: {result:?}"
    );
    assert!(
        still_recovering,
        "the unrelated paused source must still hold the global gate"
    );
    fx.wait(&a, "interrupted a", |s| {
        has_header(s, |h| h["process"]["turn_running"] == false)
    })
    .await;
    drop(gate);
    fx.close();
}

#[tokio::test]
async fn a_concurrent_draft_edit_saves_the_withdrawal_as_an_alternative() {
    let fx = Fixture::start("nd18-draft-race", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    let blobs = fx.ui().await;
    let png = include_bytes!("fixtures/pixel.png");
    let image = blobs.put_blob(png).await.unwrap();
    let file = blobs.put_blob(b"withdrawn attachment").await.unwrap();
    let image_ref =
        json!({"blob":image,"name":"before.png","media_type":"image/png","size":png.len()});
    let file_ref = json!({"blob":file,"name":"returned.txt","media_type":"text/plain","size":20});
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("race-create", "/sandbox/project", "warm up")
        .await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("finish original"));
    fx.send("active", &session, "hold").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    fx.command(
        "queued",
        "session.send",
        json!({"session":session,"text":"returned text","intent":"after_turn","attachments":[file_ref.clone()]}),
    )
    .await;
    fx.wait(&session, "written", |s| {
        prompt(s, "returned text").is_some_and(|i| i.data["state"] == "written")
    })
    .await;
    let mut ui = fx.ui().await;
    let mut before = edit_draft("draft-before", "a", &session, 0, "existing draft");
    before.args["attachments"] = json!([image_ref.clone()]);
    ui.command(&before).await.unwrap();
    std::fs::write(
        fx.scenario.root().join("runtime/backend-fault.json"),
        r#"{"contains":"cancel_async_message"}"#,
    )
    .unwrap();
    fx.command(
        "withdraw",
        "session.withdraw",
        json!({"session":session,"message":"queued"}),
    )
    .await;
    let pending = fx
        .wait(&session, "withdrawing", |s| {
            prompt(s, "returned text").is_some_and(|i| i.data["state"] == "withdrawing")
        })
        .await;
    ui.command(&edit_draft("draft-new", "b", &session, 1, "newer edit"))
        .await
        .unwrap();
    let config = fx.scenario.root().join("config.toml");
    let mut config_text = std::fs::read_to_string(&config).unwrap();
    config_text.push_str("\n[storage]\nblob_grace_seconds=0\ngc_interval_seconds=1\n");
    std::fs::write(config, config_text).unwrap();
    let orphan = blobs
        .put_blob(b"unreferenced collection witness")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while blobs.get_blob(&orphan).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        blobs.get_blob(&image).await.unwrap(),
        png,
        "in-flight return must retain the replaced draft's image"
    );
    signal(cli_pid(&fx, &pending).await, rustix::process::Signal::CONT);
    let done = fx
        .wait(&session, "withdrawn", |s| {
            prompt(s, "returned text").is_some_and(|i| i.data["state"] == "withdrawn")
        })
        .await;
    assert_eq!(draft(&done)["text"], "newer edit");
    assert_eq!(
        draft(&done)["saved"][0]["text"],
        "existing draft\n\nreturned text"
    );
    fx.command(
        "withdraw",
        "session.withdraw",
        json!({"session":session,"message":"queued"}),
    )
    .await;
    assert_eq!(
        draft(&fx.peek(&session).await)["saved"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        draft(&done)["saved"][0]["attachments"],
        json!([image_ref, file_ref])
    );
    fx.scenario.restart_daemon().unwrap();
    assert_eq!(fx.ui().await.get_blob(&image).await.unwrap(), png);
    assert_eq!(
        fx.ui().await.get_blob(&file).await.unwrap(),
        b"withdrawn attachment"
    );
    gate.release();
    fx.close();
}

#[tokio::test]
async fn withdrawal_survives_daemon_restart_before_the_cli_reply() {
    let fx = Fixture::start("nd18-recover-withdraw", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("recover-withdraw", "/sandbox/project", "warm up")
        .await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("finish original"));
    fx.send("active", &session, "keep running").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    fx.command(
        "queued",
        "session.send",
        json!({"session":session,"text":"restore after restart","intent":"after_turn"}),
    )
    .await;
    fx.wait(&session, "written", |s| {
        prompt(s, "restore after restart").is_some_and(|i| i.data["state"] == "written")
    })
    .await;
    std::fs::write(
        fx.scenario.root().join("runtime/backend-fault.json"),
        r#"{"contains":"cancel_async_message"}"#,
    )
    .unwrap();
    fx.command(
        "withdraw",
        "session.withdraw",
        json!({"session":session,"message":"queued"}),
    )
    .await;
    let pending = fx
        .wait(&session, "withdrawing", |s| {
            prompt(s, "restore after restart").is_some_and(|i| i.data["state"] == "withdrawing")
        })
        .await;
    let pid = cli_pid(&fx, &pending).await;
    fx.scenario.kill_daemon().unwrap();
    signal(pid, rustix::process::Signal::CONT);
    let restored = fx
        .wait(&session, "restored", |s| {
            prompt(s, "restore after restart").is_some_and(|i| i.data["state"] == "withdrawn")
        })
        .await;
    assert_eq!(
        restored
            .items
            .iter()
            .find(|i| i.kind == "draft")
            .unwrap()
            .data["text"],
        "restore after restart"
    );
    assert_eq!(cli_pid(&fx, &restored).await, pid);
    gate.release();
    fx.wait(&session, "finished", |s| {
        texts(s).contains(&"finish original".into())
    })
    .await;
    assert_eq!(endpoint.count(&fx.main()), 2);
    fx.close();
}

#[tokio::test]
async fn withdrawing_a_started_message_never_restores_or_resends_it() {
    let fx = Fixture::start("nd18-too-late", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("too-late", "/sandbox/project", "warm up").await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("completed normally"));
    fx.send("started", &session, "already processing").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    fx.command(
        "withdraw-started",
        "session.withdraw",
        json!({"session":session,"message":"started"}),
    )
    .await;
    let declined = fx
        .wait(&session, "withdrawal declined", |s| {
            s.items.iter().any(|i| {
                i.id == "control/withdraw-started" && i.data["state"] == "not_withdrawable"
            })
        })
        .await;
    assert_eq!(
        declined
            .items
            .iter()
            .find(|i| i.kind == "draft")
            .unwrap()
            .data["text"],
        ""
    );
    gate.release();
    fx.wait(&session, "normal result", |s| {
        texts(s).contains(&"completed normally".into())
            && prompt(s, "already processing").is_some_and(|i| i.data["state"] == "landed")
    })
    .await;
    assert_eq!(endpoint.count(&fx.main()), 2);
    fx.close();
}

#[tokio::test]
async fn explicit_stop_and_cancel_queue_restores_each_queued_message_once() {
    let fx = Fixture::start("nd18-cancel-queue", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("cancel-create", "/sandbox/project", "warm up")
        .await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("must not finish"));
    fx.send("active", &session, "hold turn").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    for (id, text) in [("queued-a", "first queued"), ("queued-b", "second queued")] {
        fx.command(
            id,
            "session.send",
            json!({"session":session,"text":text,"intent":"after_turn"}),
        )
        .await;
        fx.wait(&session, "queued", |s| {
            prompt(s, text).is_some_and(|i| i.data["state"] == "written")
        })
        .await;
    }
    fx.command(
        "cancel-queue",
        "session.interrupt",
        json!({"session":session,"queued":"cancel"}),
    )
    .await;
    let snapshot = fx
        .wait(&session, "interrupt acknowledgement", |s| {
            s.items
                .iter()
                .any(|i| i.id == "control/cancel-queue" && i.data["state"] == "acknowledged")
        })
        .await;
    assert_eq!(
        snapshot
            .items
            .iter()
            .find(|i| i.kind == "draft")
            .unwrap()
            .data["text"],
        "first queued\n\nsecond queued"
    );
    for text in ["first queued", "second queued"] {
        assert_eq!(prompt(&snapshot, text).unwrap().data["state"], "withdrawn");
    }
    assert_eq!(endpoint.count(&fx.main()), 2);
    drop(gate);
    fx.close();
}

#[tokio::test]
async fn auto_background_keeps_an_explicit_foreground_agent_completable() {
    let fx = Fixture::start("nd18-auto-agent", 3_600_000).await;
    std::fs::write(
        fx.scenario.root().join("claude/settings.json"),
        r#"{"permissions":{"allow":["Agent"]}}"#,
    )
    .unwrap();
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(),ModelReply::tool("toolu_foreground_agent","Agent",json!({"subagent_type":"general-purpose","description":"foreground timeout probe","prompt":"return AUTO_AGENT_DONE","run_in_background":false})));
    let gate = endpoint.enqueue_any_agent_held(MODEL, ModelReply::text("AUTO_AGENT_DONE"));
    endpoint.enqueue(fx.main(), ModelReply::text("agent now in background"));
    endpoint.enqueue(fx.main(), ModelReply::text("agent result received"));
    let start = tokio::time::Instant::now();
    let session = fx
        .create(
            "auto-agent-create",
            "/sandbox/project",
            "start a foreground agent",
        )
        .await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(145))
        .await
        .unwrap();
    fx.wait(&session, "automatically backgrounded", |s| {
        texts(s).contains(&"agent now in background".into())
            && has_header(s, |h| h["process"]["drain"]["drain"] == "busy")
    })
    .await;
    gate.release();
    fx.wait(&session, "agent completion", |s| {
        texts(s).contains(&"agent result received".into())
    })
    .await;
    assert!(
        endpoint
            .requests()
            .iter()
            .filter(|r| r.route.agent.is_none())
            .any(|r| request_text(&r.body).contains("AUTO_AGENT_DONE"))
    );
    eprintln!(
        "explicit foreground agent backgrounding and completion: {:?}",
        start.elapsed()
    );
    fx.close();
}

#[tokio::test]
async fn e2b_send_now_moves_foreground_mcp_to_background_and_delivers_its_result() {
    let fx = Fixture::start("nd18-e2b", 3_600_000).await;
    std::fs::write(
        fx.scenario.root().join("project/fifo_mcp.py"),
        include_str!("fixtures/fifo_mcp.py"),
    )
    .unwrap();
    std::fs::write(
        fx.scenario.root().join("project/.mcp.json"),
        r#"{"mcpServers":{"fifo":{"command":"python3","args":["/sandbox/project/fifo_mcp.py"]}}}"#,
    )
    .unwrap();
    std::fs::write(
        fx.scenario.root().join("claude/settings.json"),
        r#"{"enableAllProjectMcpServers":true,"permissions":{"allow":["mcp__fifo__wait"]}}"#,
    )
    .unwrap();
    let mut fifo = fx.scenario.fifo("mcp").unwrap();
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("e2b-create", "/sandbox/project", "warm up MCP")
        .await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    endpoint.enqueue(
        fx.main(),
        ModelReply::tool("toolu_mcp", "mcp__fifo__wait", json!({})),
    );
    fx.send("mcp-call", &session, "call foreground MCP").await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while !fx.scenario.root().join("project/mcp-started").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("real MCP must be executing before send-now");
    endpoint.enqueue(fx.main(), ModelReply::text("immediate done"));
    fx.command(
        "mcp-now",
        "session.send",
        json!({"session":session,"text":"send immediately","intent":"interrupting"}),
    )
    .await;
    let immediate = fx
        .wait(&session, "immediate result", |s| {
            texts(s).contains(&"immediate done".into())
        })
        .await;
    assert!(
        immediate.items.iter().any(|i| i.kind == "tool_result"
            && i.data["text"]
                .as_str()
                .is_some_and(|t| t.contains("moved to the background"))),
        "E2b did not background: {immediate:?}"
    );
    assert_eq!(header(&immediate)["process"]["drain"]["drain"], "busy");
    endpoint.enqueue(fx.main(), ModelReply::text("MCP result received"));
    fifo.release("finish").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fx.scenario.root().join("project/mcp-finished").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let completed = fx
        .wait(&session, "MCP result delivered", |s| {
            texts(s).contains(&"MCP result received".into())
                && has_header(s, |h| h["process"]["drain"]["drain"] == "drained")
        })
        .await;
    assert!(
        endpoint
            .requests()
            .iter()
            .any(|r| request_text(&r.body).contains("MCP_FINISHED"))
    );
    assert!(!fx.scenario.root().join("project/mcp-cancelled").exists());
    assert_eq!(
        header(&completed)["process"]["run"],
        header(&immediate)["process"]["run"]
    );
    assert_eq!(
        header(&completed)["interaction"]["immediate_preserves_mcp"],
        true
    );
    fx.close();
}

#[tokio::test]
async fn native_controls_send_withdraw_reopen_and_dispatch_escape() {
    let fx = Fixture::start("nd18-native", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("native-controls", "/sandbox/project", "warm up")
        .await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active")
    })
    .await;
    let old = endpoint.enqueue_held(fx.main(), ModelReply::text("old must not finish"));
    let next = endpoint.enqueue_held(fx.main(), ModelReply::text("new must not finish"));
    fx.send("native-active", &session, "keep running").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    let output = std::env::var_os("ND18_NATIVE_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| fx.scenario.root().join("native-controls"));
    // 输出目录可供复验复用；上一轮的协调文件不能充当本轮的时序证据。
    std::fs::create_dir_all(&output).unwrap();
    for marker in [
        "arm-withdraw",
        "armed",
        "ui-killed",
        "resumed",
        "wait-now",
        "now-started",
        "result.json",
    ] {
        match std::fs::remove_file(output.join(marker)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove stale native marker {marker}: {error}"),
        }
    }
    let mut child = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_controls.py"))
        .arg("--desktop")
        .arg(std::env::var_os("ND_TEST_DESKTOP").expect("run scripts/test-scenarios.sh"))
        .arg("--socket")
        .arg(fx.socket())
        .arg("--output")
        .arg(&output)
        .arg("--session")
        .arg(&session)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    async fn wait_file(child: &mut tokio::process::Child, path: &Path) {
        tokio::time::timeout(Duration::from_secs(40), async {
            while !path.exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "native driver exited before {}",
                    path.display()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    wait_file(&mut child, &output.join("arm-withdraw")).await;
    std::fs::write(
        fx.scenario.root().join("runtime/backend-fault.json"),
        r#"{"contains":"cancel_async_message"}"#,
    )
    .unwrap();
    std::fs::write(output.join("armed"), "").unwrap();
    wait_file(&mut child, &output.join("ui-killed")).await;
    let queued = fx.peek(&session).await;
    assert_eq!(
        prompt(&queued, "ui later").unwrap().data["state"],
        "withdrawing"
    );
    let pid = cli_pid(&fx, &queued).await;
    signal(pid, rustix::process::Signal::CONT);
    std::fs::write(output.join("resumed"), "").unwrap();
    wait_file(&mut child, &output.join("wait-now")).await;
    endpoint
        .wait_for_requests(&fx.main(), 3, Duration::from_secs(20))
        .await
        .unwrap();
    std::fs::write(output.join("now-started"), "").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(40), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(endpoint.count(&fx.main()), 3);
    let snapshot = fx.peek(&session).await;
    assert!(prompt(&snapshot, "ui fold").is_some_and(|i| i.data["state"] == "withdrawn"));
    assert!(prompt(&snapshot, "ui later").is_some_and(|i| i.data["state"] == "withdrawn"));
    assert!(!request_text(&endpoint.requests()[2].body).contains("ui later"));
    drop((old, next));
    fx.close();
}

#[tokio::test]
async fn escape_preserves_background_bash_agent_and_workflow_until_their_results_arrive() {
    for kind in ["bash", "agent", "workflow"] {
        let fx = Fixture::start(&format!("nd18-esc-{kind}"), 3_600_000).await;
        std::fs::write(
            fx.scenario.root().join("claude/settings.json"),
            r#"{"permissions":{"allow":["Bash","Agent","Workflow"]}}"#,
        )
        .unwrap();
        let endpoint = fx.scenario.endpoint();
        let mut fifo = fx.scenario.fifo("background").unwrap();
        let tool = match kind {
            "bash" => ModelReply::tool(
                "toolu_background",
                "Bash",
                json!({"command":format!("head -n 1 {}",fifo.sandbox_path().display()),"run_in_background":true,"description":"background FIFO"}),
            ),
            "agent" => ModelReply::tool(
                "toolu_background",
                "Agent",
                json!({"prompt":"return BACKGROUND_RESULT","subagent_type":"general-purpose","description":"background agent","run_in_background":true}),
            ),
            "workflow" => ModelReply::tool(
                "toolu_background",
                "Workflow",
                json!({"script":"export const meta = { name: 'nd-background', description: 'offline background agent' };\nconst result = await agent('return BACKGROUND_RESULT', { label: 'background' });\nreturn { result };"}),
            ),
            _ => unreachable!(),
        };
        endpoint.enqueue(fx.main(), tool);
        let agent_gate =
            endpoint.enqueue_any_agent_held(MODEL, ModelReply::text("BACKGROUND_RESULT"));
        let turn_gate =
            endpoint.enqueue_held(fx.main(), ModelReply::text("never finish foreground"));
        for _ in 0..5 {
            endpoint.enqueue(fx.main(), ModelReply::text("received background result"));
        }
        let session = fx
            .create(
                "background-create",
                "/sandbox/project",
                "start background work",
            )
            .await;
        endpoint
            .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
            .await
            .unwrap();
        if kind != "bash" {
            tokio::time::timeout(Duration::from_secs(30), async {
                while !endpoint.requests().iter().any(|r| r.route.agent.is_some()) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        }
        fx.wait(&session, "background running", |s| {
            has_header(s, |h| h["process"]["drain"]["drain"] == "busy")
        })
        .await;
        fx.command(
            "background-esc",
            "session.interrupt",
            json!({"session":session}),
        )
        .await;
        let stopped = fx
            .wait(&session, "foreground ended", |s| {
                has_header(s, |h| h["process"]["turn_running"] == false)
                    && s.items.iter().any(|i| i.kind == "turn")
            })
            .await;
        assert_eq!(
            header(&stopped)["process"]["drain"]["drain"],
            "busy",
            "{kind}: background was stopped"
        );
        drop(turn_gate);
        if kind == "bash" {
            fifo.release("BACKGROUND_RESULT").unwrap();
        }
        agent_gate.release();
        fx.wait(&session, "background result delivered", |s| {
            texts(s).contains(&"received background result".into())
        })
        .await;
        let delivered = endpoint
            .requests()
            .iter()
            .filter(|r| r.route.agent.is_none())
            .map(|r| request_text(&r.body))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            delivered.contains("task-notification") && delivered.contains("completed"),
            "{kind}: {delivered}"
        );
        if kind != "bash" {
            assert!(
                delivered.contains("BACKGROUND_RESULT"),
                "{kind}: {delivered}"
            );
        }
        fx.wait(&session, "background drained", |s| {
            has_header(s, |h| h["process"]["drain"]["drain"] == "drained")
        })
        .await;
        fx.close();
    }
}

#[tokio::test]
async fn send_intents_land_at_the_requested_turn_boundary() {
    for intent in ["fold", "after_turn", "interrupting"] {
        let fx = Fixture::start(&format!("nd18-{intent}").replace('_', "-"), 3_600_000).await;
        std::fs::write(
            fx.scenario.root().join("claude/settings.json"),
            r#"{"permissions":{"allow":["Bash"]}}"#,
        )
        .unwrap();
        let endpoint = fx.scenario.endpoint();
        endpoint.enqueue(fx.main(), ModelReply::text("ready"));
        let session = fx
            .create("intent-create", "/sandbox/project", "warm up")
            .await;
        fx.wait(&session, "created", |s| {
            has_header(s, |h| h["status"] == "active") && texts(s).contains(&"ready".into())
        })
        .await;
        let mut fifo = fx.scenario.fifo("boundary").unwrap();
        endpoint.enqueue(fx.main(), ModelReply::tool("toolu_boundary", "Bash", json!({"command":format!("printf started > /sandbox/project/started; head -n 1 {}", fifo.sandbox_path().display()), "description":"turn boundary"})));
        endpoint.enqueue(fx.main(), ModelReply::text("continuation"));
        if intent == "after_turn" {
            endpoint.enqueue(fx.main(), ModelReply::text("later turn"));
        }
        fx.send("active", &session, "active turn").await;
        endpoint
            .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while !fx.scenario.root().join("project/started").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        fx.command(
            "followup",
            "session.send",
            json!({"session":session,"text":"FOLLOWUP_MARKER","intent":intent}),
        )
        .await;
        if intent == "interrupting" {
            // 新输入必须在前台 Bash 仍阻塞时抵达，不等待 FIFO 放行。
            endpoint
                .wait_for_requests(&fx.main(), 3, Duration::from_secs(30))
                .await
                .unwrap();
        } else {
            fx.wait(&session, "written followup", |s| {
                prompt(s, "FOLLOWUP_MARKER").is_some_and(|p| p.data["state"] == "written")
            })
            .await;
            fifo.release("continue").unwrap();
        }
        let expected = if intent == "after_turn" {
            "later turn"
        } else {
            "continuation"
        };
        let snapshot = fx
            .wait(&session, "intended turn completed", |s| {
                texts(s).contains(&expected.into())
                    && prompt(s, "FOLLOWUP_MARKER").is_some_and(|p| p.data["state"] == "landed")
                    && has_header(s, |h| h["process"]["turn_running"] == false)
            })
            .await;
        let requests = endpoint.requests();
        assert_eq!(requests.len(), if intent == "after_turn" { 4 } else { 3 });
        assert_eq!(
            request_text(&requests[2].body).contains("FOLLOWUP_MARKER"),
            intent != "after_turn"
        );
        if intent == "after_turn" {
            assert!(request_text(&requests[3].body).contains("FOLLOWUP_MARKER"));
        }
        let rounds = snapshot
            .items
            .iter()
            .find(|i| i.kind == "lineage")
            .unwrap()
            .data["rounds"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            rounds.len(),
            if intent == "fold" { 2 } else { 3 },
            "{intent}: {rounds:?}"
        );
        fx.close();
    }
}

#[tokio::test]
async fn withdrawn_queued_text_returns_to_a_durable_draft_after_ui_disappears() {
    let fx = Fixture::start("nd18-withdraw", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx
        .create("withdraw-create", "/sandbox/project", "warm up")
        .await;
    fx.wait(&session, "created", |s| {
        has_header(s, |h| h["status"] == "active") && texts(s).contains(&"ready".into())
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("first done"));
    fx.send("first", &session, "first prompt").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    fx.command(
        "queued",
        "session.send",
        json!({"session":session,"text":"return this text","intent":"after_turn"}),
    )
    .await;
    fx.wait(&session, "queued input written", |s| {
        prompt(s, "return this text").is_some_and(|p| p.data["state"] == "written")
    })
    .await;
    let mut ui = fx.ui().await;
    let command = Command {
        id: "withdraw".into(),
        device: "test".into(),
        name: "session.withdraw".into(),
        args: json!({"session":session,"message":"queued"}),
        expect: json!({}),
    };
    let receipt = ui.command(&command).await.unwrap();
    assert!(
        matches!(
            receipt,
            CommandReply::Receipt {
                receipt: Receipt::Done { .. }
            }
        ),
        "{receipt:?}"
    );
    drop(ui); // 界面在撤回结果到达之前消失，冷副本必须恢复同一个持久稿。
    let snapshot = fx
        .wait(&session, "withdrawn and restored", |s| {
            prompt(s, "return this text").is_some_and(|p| p.data["state"] == "withdrawn")
        })
        .await;
    let draft = snapshot.items.iter().find(|i| i.kind == "draft").unwrap();
    assert_eq!(draft.data["text"], "return this text");
    assert_eq!(fx.ui().await.command(&command).await.unwrap(), receipt);
    fx.command("withdraw-again", "session.withdraw", command.args.clone())
        .await;
    let again = fx.peek(&session).await;
    assert_eq!(
        again.items.iter().find(|i| i.kind == "draft").unwrap().data,
        draft.data
    );
    gate.release();
    fx.wait(&session, "first turn completes", |s| {
        texts(s).contains(&"first done".to_string())
    })
    .await;
    assert_eq!(
        endpoint.count(&fx.main()),
        2,
        "withdrawn text must never reach the model"
    );
    fx.close();
}

#[tokio::test]
async fn escape_ends_the_turn_without_ending_the_backend() {
    let fx = Fixture::start("nd18-escape", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("must not finish"));
    let session = fx
        .create("esc-create", "/sandbox/project", "hold this turn")
        .await;
    endpoint
        .wait_for_requests(&fx.main(), 1, Duration::from_secs(30))
        .await
        .unwrap();
    let before = fx
        .wait(&session, "running", |s| {
            has_header(s, |h| h["process"]["turn_running"] == true)
        })
        .await;
    let reply = fx
        .command("esc", "session.interrupt", json!({"session":session}))
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
    let after = fx
        .wait(&session, "turn ended", |s| {
            has_header(s, |h| h["process"]["turn_running"] == false)
                && s.items.iter().any(|i| i.kind == "turn")
        })
        .await;
    assert_eq!(
        header(&before)["process"]["run"],
        header(&after)["process"]["run"]
    );
    assert_eq!(header(&after)["process"]["alive"], true);
    assert!(!texts(&after).iter().any(|s| s == "must not finish"));
    drop(gate);
    fx.close();
}

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
        Self::with_env(name, idle_reclaim_ms, "").await
    }
    async fn with_env(name: &str, idle_reclaim_ms: u64, extra_env: &str) -> Self {
        let auto_title = name.starts_with("nd21-title-ai");
        let poll_timeout_ms = if name == "nd20-thousand" { 20 } else { 5000 };
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
poll_timeout_ms = {poll_timeout_ms}
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
{extra_env}

[sessions]
auto_title = {auto_title}
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
                "timed out waiting for {what}: {:#?}; requests={:?}",
                snapshot.items,
                self.scenario
                    .endpoint()
                    .requests()
                    .iter()
                    .map(|r| (&r.route, &r.body["max_tokens"]))
                    .collect::<Vec<_>>()
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

fn draft(snapshot: &Snapshot) -> &Value {
    &snapshot
        .items
        .iter()
        .find(|i| i.id == "draft")
        .expect("session draft")
        .data
}

fn edit_draft(id: &str, device: &str, session: &str, version: u64, text: &str) -> Command {
    Command {
        id: id.into(),
        device: device.into(),
        name: "session.draft.update".into(),
        args: json!({"session":session,"text":text}),
        expect: json!({"draft_version":version}),
    }
}

#[tokio::test]
async fn draft_survives_ui_close_and_daemon_crash() {
    let fx = Fixture::start("nd16-draft-reopen", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("draft-create", "/sandbox/project", "hello").await;
    fx.wait(&session, "first turn", |s| !texts(s).is_empty())
        .await;
    let mut ui = fx.ui().await;
    let command = edit_draft(
        "draft-edit",
        "desktop-a",
        &session,
        0,
        "未发送的中文\n第二行 🦀",
    );
    let reply = ui.command(&command).await.unwrap();
    assert!(
        matches!(
            reply,
            CommandReply::Receipt {
                receipt: Receipt::Done { .. }
            }
        ),
        "{reply:?}"
    );
    ui.close().await.unwrap();
    let before = fx.peek(&session).await;
    assert_eq!(draft(&before)["text"], "未发送的中文\n第二行 🦀");
    assert_eq!(draft(&before)["version"], 1);
    fx.scenario.kill_daemon().unwrap();
    let mut reopened = fx.scenario.connect().await.unwrap();
    let after = reopened
        .subscribe(&format!("session/{session}"))
        .await
        .unwrap();
    assert_eq!(draft(&after), draft(&before));
    assert_eq!(
        reopened.command(&command).await.unwrap(),
        reply,
        "retry returns original receipt"
    );
    let client = nd_ui_core::CommandClient::start(fx.socket()).unwrap();
    assert!(matches!(
        client.receipt(command.id.clone()).await.unwrap(),
        nd_wire::ReceiptLookup::Found {
            receipt: Receipt::Done { .. }
        }
    ));
    assert_eq!(
        client.receipt("never-submitted".into()).await.unwrap(),
        nd_wire::ReceiptLookup::Missing
    );
    assert_eq!(
        draft(&fx.peek(&session).await)["version"],
        1,
        "receipt recovery cannot reissue an edit"
    );
    assert_eq!(
        fx.scenario.endpoint().requests().len(),
        1,
        "drafts never prompt the model"
    );
    fx.close();
}

#[tokio::test]
async fn draft_two_devices_preserve_the_loser_and_publish_it_to_both() {
    let fx = Fixture::start("nd16-draft-conflict", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("draft-pair", "/sandbox/project", "hello").await;
    let first = fx
        .wait(&session, "first turn", |s| !texts(s).is_empty())
        .await;
    let stream = format!("session/{session}");
    let mut a = fx.ui().await;
    let mut b = fx.ui().await;
    assert_eq!(draft(&a.subscribe(&stream).await.unwrap())["version"], 0);
    assert_eq!(draft(&b.subscribe(&stream).await.unwrap())["version"], 0);
    let ca = edit_draft("draft-a", "desktop-a", &session, 0, "A 的草稿");
    let cb = edit_draft("draft-b", "desktop-b", &session, 0, "B 的草稿");
    let (ra, rb) = tokio::join!(a.command(&ca), b.command(&cb));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    let sa = a.subscribe(&stream).await.unwrap();
    let sb = b.subscribe(&stream).await.unwrap();
    assert_eq!(draft(&sa), draft(&sb));
    let d = draft(&sa);
    assert_eq!(d["version"], 1);
    let saved = d["saved"].as_array().expect("saved drafts");
    assert_eq!(saved.len(), 1);
    let (winner, loser, loser_id, loser_device) = if d["text"] == "A 的草稿" {
        (&ra, &rb, "draft-b", "desktop-b")
    } else {
        (&rb, &ra, "draft-a", "desktop-a")
    };
    assert!(
        matches!(winner, CommandReply::Receipt { receipt: Receipt::Done { value } } if value["saved"].is_null())
    );
    assert!(
        matches!(loser, CommandReply::Receipt { receipt: Receipt::Done { value } } if value["saved"] == loser_id)
    );
    assert_eq!(saved[0]["id"], loser_id);
    assert_eq!(saved[0]["device"], loser_device);
    assert_eq!(saved[0]["base_version"], 0);
    assert_ne!(saved[0]["text"], d["text"]);
    assert_eq!(a.command(&ca).await.unwrap(), ra);
    assert_eq!(b.command(&cb).await.unwrap(), rb);
    let mut changed = cb.clone();
    changed.args["text"] = json!("同 id 改内容");
    assert_eq!(b.command(&changed).await.unwrap(), CommandReply::Conflict);
    // 暂停真实 CLI，保证两台界面都在恢复窗口内订阅，不靠调度碰巧复现。
    let pid = cli_pid(&fx, &first).await;
    signal(pid, rustix::process::Signal::STOP);
    fx.scenario.restart_daemon().unwrap();
    assert_eq!(draft(&fx.peek(&session).await), d);
    // 取回落败稿仍是一次带版本的普通编辑，成功后两台通过事件同步。
    let restore = edit_draft(
        "draft-restore",
        "desktop-b",
        &session,
        1,
        saved[0]["text"].as_str().unwrap(),
    );
    let mut watcher = fx.ui().await;
    let mut other_watcher = fx.ui().await;
    for reader in [&mut watcher, &mut other_watcher] {
        let cold = reader.subscribe(&stream).await.unwrap();
        assert_ne!(cold.epoch, sa.epoch, "restart starts a new epoch");
        assert_eq!(header(&cold)["recovering"], true);
        assert_eq!(draft(&cold), d, "both devices recover the saved drafts");
    }
    signal(pid, rustix::process::Signal::CONT);
    let restored = fx.ui().await.command(&restore).await.unwrap();
    assert!(matches!(
        restored,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    for reader in [&mut watcher, &mut other_watcher] {
        // next() 返回任意事件应用后的完整副本。恢复头部可以先变化，
        // 此时草稿仍是版本 1；按业务条件等待，不能假定下一次就是草稿编辑。
        let event = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = reader.next().await.unwrap();
                if draft(&snapshot)["version"] == 2 {
                    break snapshot;
                }
                assert_eq!(draft(&snapshot), d, "recovery cannot change saved drafts");
            }
        })
        .await
        .expect("both subscribers must receive the restored draft");
        assert_eq!(draft(&event)["text"], saved[0]["text"]);
        assert_eq!(
            draft(&event)["saved"],
            d["saved"],
            "recovery keeps the saved original"
        );
    }
    assert_eq!(fx.ui().await.command(&restore).await.unwrap(), restored);
    assert_eq!(draft(&fx.peek(&session).await)["version"], 2);
    assert_eq!(
        fx.scenario.endpoint().requests().len(),
        1,
        "draft recovery never prompts the model"
    );
    fx.close();
}

#[tokio::test]
async fn draft_send_consumes_only_the_matching_version_atomically() {
    let fx = Fixture::start("nd16-draft-send", 3_600_000).await;
    for answer in ["ready", "sent", "again"] {
        fx.scenario
            .endpoint()
            .enqueue(fx.main(), ModelReply::text(answer));
    }
    let session = fx
        .create("draft-send-create", "/sandbox/project", "hello")
        .await;
    fx.wait(&session, "first turn", |s| !texts(s).is_empty())
        .await;
    let mut ui = fx.ui().await;
    ui.command(&edit_draft("to-send", "a", &session, 0, "发出去"))
        .await
        .unwrap();
    let send = Command {
        id: "send-draft".into(),
        device: "a".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"发出去"}),
        expect: json!({"draft_version":1}),
    };
    let reply = ui.command(&send).await.unwrap();
    assert!(matches!(
        reply,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    let cleared = fx.peek(&session).await;
    assert_eq!(draft(&cleared)["text"], "");
    assert_eq!(draft(&cleared)["version"], 2);
    ui.command(&edit_draft("newer", "b", &session, 2, "保留的新草稿"))
        .await
        .unwrap();
    assert_eq!(ui.command(&send).await.unwrap(), reply);
    let mut stale_send = send.clone();
    stale_send.id = "send-stale".into();
    ui.command(&stale_send).await.unwrap();
    let retained = fx.peek(&session).await;
    assert_eq!(draft(&retained)["text"], "保留的新草稿");
    assert_eq!(draft(&retained)["version"], 3);
    // 缺版本的编辑不能成为无条件覆盖；非法发送不能清草稿。
    let mut invalid = edit_draft("bad-edit", "a", &session, 3, "丢弃");
    invalid.expect = json!({});
    assert!(
        matches!(ui.command(&invalid).await.unwrap(), CommandReply::Receipt { receipt: Receipt::Rejected { code, .. } } if code == "invalid")
    );
    stale_send.id = "empty-send".into();
    stale_send.args["text"] = json!("");
    stale_send.expect = json!({"draft_version":3});
    ui.command(&stale_send).await.unwrap();
    assert_eq!(draft(&fx.peek(&session).await), draft(&retained));
    fx.close();
}

#[tokio::test]
async fn draft_native_windows_save_reopen_follow_and_recover() {
    let fx = Fixture::start("nd16-native-drafts", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("sent"));
    let session = fx
        .create("native-drafts", "/sandbox/project", "hello")
        .await;
    fx.wait(&session, "first turn", |s| !texts(s).is_empty())
        .await;
    fx.ui()
        .await
        .command(&edit_draft(
            "native-loser",
            "test",
            &session,
            99,
            "可找回的落败稿",
        ))
        .await
        .unwrap();
    let output = std::env::var_os("ND_NATIVE_DRAFT_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| fx.scenario.root().join("native-drafts"));
    let child = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
        .args([
            "--desktop",
            &std::env::var("ND_TEST_DESKTOP").expect("build scenarios desktop"),
        ])
        .arg("--daemon-outage")
        .arg("--socket")
        .arg(fx.socket())
        .arg("--session")
        .arg(&session)
        .arg("--output")
        .arg(&output)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for (marker, stop) in [("stop-daemon", true), ("start-daemon", false)] {
        tokio::time::timeout(Duration::from_secs(60), async {
            while !output.join(marker).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("native window must reach the daemon outage gate");
        if stop {
            fx.scenario.stop_daemon().unwrap();
            std::fs::write(output.join("edit-during-outage"), "").unwrap();
        } else {
            fx.scenario.start_daemon().unwrap();
        }
    }
    let result = child.wait_with_output().await.unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let snapshot = fx
        .wait(&session, "native draft sent", |s| {
            texts(s).contains(&"sent".into())
        })
        .await;
    assert_eq!(draft(&snapshot)["text"], "");
    assert_eq!(fx.scenario.endpoint().requests().len(), 2);
    fx.close();
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
    native_chat(false).await;
}

#[tokio::test]
async fn native_theme_change_preserves_streaming_chat_and_draft() {
    native_chat(true).await;
}

#[tokio::test]
async fn native_theme_files_selection_and_system_appearance() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("ND_NATIVE_THEME_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().join("themes"));
    let desktop = std::env::var_os("ND_TEST_DESKTOP").expect("run scripts/test-scenarios.sh");
    let result = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_themes.py"))
        .arg("--bin-dir")
        .arg(Path::new(&desktop).parent().unwrap())
        .arg("--output")
        .arg(output)
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

async fn native_chat(themes: bool) {
    let fx = Fixture::start(
        if themes { "nd23-window" } else { "nd14-window" },
        3_600_000,
    )
    .await;
    let answer = "# 中文回答\n\n一段 **Markdown**。\n\n```rust\nfn main() { println!(\"你好\"); }\n```\n\n结束。";
    let answer = if themes {
        "# 中文回答\n\n[主题链接 LINK](https://example.invalid) 和 `INLINE_CODE`\n\n| 表头 HEAD | 第二列 |\n| --- | --- |\n| 内容 | 内容 |\n\n```rust\nfn main() { println!(\"你好\"); }\n```\n\n结束。"
    } else {
        answer
    };
    let finish = fx.scenario.endpoint().enqueue_finish_held(
        Route::new(None, "claude-haiku-4-5-20251001"),
        ModelReply::streaming_text(answer, 1, 10),
    );
    let desktop = std::env::var_os("ND_TEST_DESKTOP").expect("run scripts/test-scenarios.sh");
    let output = std::env::var_os(if themes {
        "ND_NATIVE_THEME_CHAT_OUTPUT"
    } else {
        "ND_NATIVE_OUTPUT"
    })
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|| fx.scenario.root().join("native-chat"));
    let child = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
        .arg("--desktop")
        .arg(desktop)
        .arg("--socket")
        .arg(fx.socket())
        .arg("--output")
        .arg(&output)
        .args(if themes { vec!["--themes"] } else { vec![] })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(45), async {
        while !output.join("release-stream").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native scenario must observe an unfinished cold block before release");
    finish.release();
    let result = child.wait_with_output().await.unwrap();
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

#[tokio::test]
async fn restart_keeps_a_written_message_pending_until_its_original_echo() {
    let fx = Fixture::start("nd19-written", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("第一轮"));
    let session = fx.create("nd19-create", "/sandbox/project", "开始").await;
    let first = fx
        .wait(&session, "first answer", |s| texts(s) == ["第一轮"])
        .await;
    let pid = cli_pid(&fx, &first).await;
    signal(pid, rustix::process::Signal::STOP);
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("第二轮"));
    fx.send("nd19-send", &session, "只送一次").await;
    let written = fx
        .wait(&session, "written", |s| {
            prompt(s, "只送一次").is_some_and(|p| p.data["state"] == "written")
        })
        .await;
    let native = prompt(&written, "只送一次").unwrap().data["native"].clone();
    // 给已提交检查点的确认时间，确保测试覆盖流水回收后的恢复。
    tokio::time::sleep(Duration::from_millis(300)).await;
    fx.scenario.restart_daemon().unwrap();
    let mut ui = fx.ui().await;
    ui.subscribe(&format!("session/{session}")).await.unwrap();
    signal(pid, rustix::process::Signal::CONT);
    let done = fx
        .wait(&session, "original echo", |s| {
            prompt(s, "只送一次").is_some_and(|p| p.data["state"] == "landed")
        })
        .await;
    assert_eq!(prompt(&done, "只送一次").unwrap().data["native"], native);
    assert_eq!(cli_pid(&fx, &done).await, pid);
    fx.wait(&session, "second answer", |s| {
        texts(s) == ["第一轮", "第二轮"]
    })
    .await;
    let bs = header(&done)["process"]["backend_session"]
        .as_str()
        .unwrap();
    let inputs: Vec<Value> = fx
        .transcript(bs)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &Value| {
            v["type"] == "user" && v["message"]["content"].to_string().contains("只送一次")
        })
        .collect();
    assert_eq!(inputs.len(), 1, "no duplicated native input: {inputs:?}");
    assert_eq!(fx.scenario.endpoint().requests().len(), 2);
    fx.close();
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
    // 已拉起的后端会话属于根段；没等到回显的首条提示还不是轮。
    assert_eq!(lineage(&written)["segments"].as_array().unwrap().len(), 1);
    assert!(lineage(&written)["rounds"].as_array().unwrap().is_empty());
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
    let run_dir = fx
        .scenario
        .watchdogs()
        .unwrap()
        .directory(&first_run)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while run_dir.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("committed Gone run must release its runtime spool directory");
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

fn lineage(snapshot: &Snapshot) -> &Value {
    &snapshot
        .items
        .iter()
        .find(|i| i.id == "lineage")
        .expect("lineage snapshot")
        .data
}

#[tokio::test]
async fn human_rounds_map_to_cli_uuids_and_survive_restart() {
    let fx = Fixture::start("nd15-rounds", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("第一轮回答"));
    let session = fx
        .create("rounds-create", "/sandbox/project", "第一条提示")
        .await;
    let first = fx
        .wait(&session, "first round complete", |s| {
            texts(s) == ["第一轮回答"] && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let round = lineage(&first)["rounds"][0].clone();
    assert_eq!(round["n"], 1);
    assert_eq!(round["complete"], true);
    assert_eq!(
        round["positions"][0]["native"],
        prompt(&first, "第一条提示").unwrap().data["native"]
    );
    let bs = header(&first)["process"]["backend_session"]
        .as_str()
        .unwrap();
    let record: Vec<Value> = fx
        .transcript(bs)
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(
        record
            .iter()
            .any(|r| r["type"] == "user" && r["uuid"] == round["positions"][0]["native"])
    );
    assert!(
        record
            .iter()
            .any(|r| r["type"] == "assistant" && r["uuid"] == round["last_assistant"]["native"])
    );
    fx.scenario.kill_daemon().unwrap();
    let recovered = fx.peek(&session).await;
    assert_eq!(lineage(&recovered), lineage(&first));
    let pid = cli_pid(&fx, &recovered).await;
    signal(pid, rustix::process::Signal::STOP);
    endpoint.enqueue(fx.main(), ModelReply::text("第二轮回答"));
    fx.send("rounds-send", &session, "第二条提示").await;
    let written = fx
        .wait(&session, "written before echo", |s| {
            prompt(s, "第二条提示").is_some_and(|p| p.data["state"] == "written")
        })
        .await;
    assert_eq!(lineage(&written)["rounds"].as_array().unwrap().len(), 1);
    signal(pid, rustix::process::Signal::CONT);
    let ended = fx
        .wait(&session, "second round complete", |s| {
            texts(s) == ["第一轮回答", "第二轮回答"]
                && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let rounds = lineage(&ended)["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0], round);
    assert_eq!(rounds[1]["n"], 2);
    assert_eq!(rounds[1]["messages"], json!(["rounds-send"]));
    assert_eq!(
        rounds[1]["positions"][0]["native"],
        prompt(&ended, "第二条提示").unwrap().data["native"]
    );
    assert_ne!(rounds[0]["id"], rounds[1]["id"]);
    fx.close();
}

#[tokio::test]
async fn coalesced_cli_prompts_share_one_navigation_round() {
    let fx = Fixture::start("nd15-coalesced", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("准备好了"));
    let session = fx
        .create("coalesce-create", "/sandbox/project", "准备")
        .await;
    let first = fx
        .wait(&session, "first round done", |s| {
            texts(s) == ["准备好了"] && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let pid = cli_pid(&fx, &first).await;
    signal(pid, rustix::process::Signal::STOP);
    endpoint.enqueue(fx.main(), ModelReply::text("两条一起回答"));
    fx.send("coalesce-a", &session, "同时写出的第一条").await;
    fx.send("coalesce-b", &session, "同时写出的第二条").await;
    let written = fx
        .wait(&session, "both written", |s| {
            ["同时写出的第一条", "同时写出的第二条"]
                .iter()
                .all(|text| prompt(s, text).is_some_and(|p| p.data["state"] == "written"))
        })
        .await;
    assert_eq!(lineage(&written)["rounds"].as_array().unwrap().len(), 1);
    signal(pid, rustix::process::Signal::CONT);
    let ended = fx
        .wait(&session, "coalesced round done", |s| {
            texts(s) == ["准备好了", "两条一起回答"]
                && has_header(s, |h| h["process"]["turn_running"] == false)
        })
        .await;
    let rounds = lineage(&ended)["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 2, "{rounds:#?}");
    assert_eq!(rounds[1]["messages"], json!(["coalesce-a", "coalesce-b"]));
    let natives: Vec<_> = ["同时写出的第一条", "同时写出的第二条"]
        .iter()
        .map(|text| prompt(&ended, text).unwrap().data["native"].clone())
        .collect();
    assert_eq!(
        rounds[1]["positions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["native"].clone())
            .collect::<Vec<_>>(),
        natives
    );
    assert_eq!(endpoint.requests().len(), 2);
    let request = request_text(&endpoint.requests()[1].body);
    assert!(request.contains("同时写出的第一条") && request.contains("同时写出的第二条"));
    fx.close();
}

#[tokio::test]
async fn a_running_round_keeps_its_identity_when_the_daemon_restarts() {
    let fx = Fixture::start("nd15-running", 3_600_000).await;
    let answer = "流式回答跨过守护进程重启以后仍然属于同一轮".repeat(5);
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::streaming_text(&answer, 1, 80));
    let session = fx
        .create("running-round", "/sandbox/project", "慢慢回答")
        .await;
    let running = fx
        .wait(&session, "running round indexed", |s| {
            s.items
                .iter()
                .find(|i| i.id == "lineage")
                .is_some_and(|i| i.data["rounds"][0]["complete"] == false)
        })
        .await;
    let id = lineage(&running)["rounds"][0]["id"].clone();
    fx.scenario.kill_daemon().unwrap();
    let ended = fx
        .wait(&session, "same round complete after restart", |s| {
            s.items
                .iter()
                .find(|i| i.id == "lineage")
                .is_some_and(|i| i.data["rounds"][0]["complete"] == true)
        })
        .await;
    let rounds = lineage(&ended)["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0]["id"], id);
    assert_eq!(
        rounds[0]["positions"],
        lineage(&running)["rounds"][0]["positions"]
    );
    assert_eq!(fx.scenario.endpoint().requests().len(), 1);
    fx.close();
}

#[tokio::test]
async fn recovering_commands_have_no_receipt_and_the_replica_retries_the_same_id() {
    let fx = Fixture::start("nd19-gate", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("就绪"));
    let session = fx.create("gate-create", "/sandbox/project", "开始").await;
    let first = fx.wait(&session, "ready", |s| texts(s) == ["就绪"]).await;
    let pid = cli_pid(&fx, &first).await;
    signal(pid, rustix::process::Signal::STOP); // 两个 mod 暂时不能重新 hello。
    fx.scenario.restart_daemon().unwrap();
    let mut ui = fx.ui().await;
    // 新建会话也受守护进程恢复闸门约束，不能绕过正在恢复的会话。
    let create = Command {
        id: "create-during-gate".into(),
        device: "test".into(),
        name: "session.create".into(),
        args: json!({"cwd":"/sandbox/project","text":"恢复期不能新建","model":MODEL}),
        expect: json!({}),
    };
    let mut other = fx.ui().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(250), other.command(&create))
            .await
            .is_err()
    );
    assert_eq!(
        fx.ui().await.receipt(&create.id).await.unwrap(),
        nd_wire::ReceiptLookup::Missing
    );
    let client = nd_ui_core::CommandClient::start(fx.socket()).unwrap();
    let abandoned = Command {
        id: "cancelled-during-gate".into(),
        device: "test".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"界面已经取消的请求"}),
        expect: json!({}),
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(200), client.command(abandoned))
            .await
            .is_err()
    );
    drop(client);
    let command = Command {
        id: "same-id-during-recovery".into(),
        device: "test".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"恢复后发送"}),
        expect: json!({}),
    };
    let task_command = command.clone();
    let task = tokio::spawn(async move { ui.command(&task_command).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(1800)).await;
    assert_eq!(
        fx.ui().await.receipt(&command.id).await.unwrap(),
        nd_wire::ReceiptLookup::Missing
    );
    assert!(
        !task.is_finished(),
        "recovery retries must outlast the previous five attempts"
    );
    assert_eq!(fx.scenario.endpoint().requests().len(), 1);
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("恢复后的回答"));
    signal(pid, rustix::process::Signal::CONT);
    let reply = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        reply,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    assert_eq!(fx.ui().await.command(&command).await.unwrap(), reply);
    fx.wait(&session, "one resumed input", |s| {
        texts(s) == ["就绪", "恢复后的回答"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1800)).await;
    assert_eq!(
        fx.ui()
            .await
            .receipt("cancelled-during-gate")
            .await
            .unwrap(),
        nd_wire::ReceiptLookup::Missing
    );
    assert_eq!(fx.scenario.endpoint().requests().len(), 2);
    fx.close();
}

#[tokio::test]
async fn streaming_survives_kill_and_service_restart_with_a_checkpoint_mid_block() {
    for kill in [true, false] {
        let fx = Fixture::start(
            if kill {
                "nd19-stream-kill"
            } else {
                "nd19-stream-restart"
            },
            3_600_000,
        )
        .await;
        let answer: String = (0..100).map(|n| format!("{n:03}·")).collect();
        fx.scenario
            .endpoint()
            .enqueue(fx.main(), ModelReply::streaming_text(&answer, 1, 30));
        fx.scenario
            .endpoint()
            .enqueue(fx.main(), ModelReply::text("接着聊"));
        let session = fx
            .create("stream-create", "/sandbox/project", "长回复")
            .await;
        let partial = fx
            .wait(&session, "partial answer", |s| {
                s.items.iter().any(|i| {
                    i.kind == "text"
                        && i.data["complete"] == false
                        && i.data["text"].as_str().unwrap().len() > 20
                })
            })
            .await;
        let block = partial.items.iter().find(|i| i.kind == "text").unwrap();
        let pid = cli_pid(&fx, &partial).await;
        // 在流式块中制造持久事实：第二条输入已写出但排在当前回合之后。
        fx.command(
            "stream-queued",
            "session.send",
            json!({"session":session,"text":"下一轮","intent":"after_turn"}),
        )
        .await;
        fx.wait(&session, "queued input checkpoint", |s| {
            prompt(s, "下一轮").is_some_and(|i| i.data["state"] == "written")
        })
        .await;
        let mut ui = fx.ui().await;
        ui.subscribe(&format!("session/{session}")).await.unwrap();
        if kill {
            fx.scenario.kill_daemon().unwrap();
        } else {
            fx.scenario.restart_daemon().unwrap();
        }
        let resumed = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let s = ui.next().await.unwrap();
                if s.epoch != partial.epoch
                    && header(&s)["recovering"] == false
                    && s.items.iter().any(|i| {
                        i.id == block.id
                            && i.data["complete"] == false
                            && i.data["text"].as_str().unwrap().len() > 25
                    })
                {
                    break s;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            fx.scenario.root().join("runtime/runs").is_dir(),
            "daemon restart must preserve watchdog runtime files"
        );
        let text = resumed
            .items
            .iter()
            .find(|i| i.id == block.id)
            .unwrap()
            .data["text"]
            .as_str()
            .unwrap();
        assert!(
            text.starts_with(block.data["text"].as_str().unwrap()),
            "lost streamed prefix after restart: {text}"
        );
        // 最后一个 delta 已可含完整正文，但 assistant 完整块和 result 还可能在途。
        // 等公开的完成事实齐全，再核对两轮恰好各一次。
        let done = fx
            .wait(&session, "both replies and round results completed", |s| {
                texts(s) == [answer.clone(), "接着聊".into()]
                    && s.items
                        .iter()
                        .filter(|i| i.kind == "text")
                        .all(|i| i.data["complete"] == true)
                    && s.items.iter().filter(|i| i.kind == "turn").count() >= 2
                    && header(s)["process"]["turn_running"] == false
            })
            .await;
        assert_eq!(cli_pid(&fx, &done).await, pid);
        assert_eq!(done.items.iter().filter(|i| i.id == block.id).count(), 1);
        assert_eq!(done.items.iter().filter(|i| i.kind == "turn").count(), 2);
        assert_eq!(fx.scenario.endpoint().requests().len(), 2);
        fx.close();
    }
}

#[tokio::test]
async fn ambiguous_write_and_crash_before_accounting_never_resend_the_native_input() {
    for action in ["unknown_after_write", "crash_after_write"] {
        let fx = Fixture::start("nd19-write-window", 3_600_000).await;
        fx.scenario
            .endpoint()
            .enqueue(fx.main(), ModelReply::text("第一轮"));
        let session = fx.create("window-create", "/sandbox/project", "开始").await;
        let first = fx.wait(&session, "ready", |s| texts(s) == ["第一轮"]).await;
        let pid = cli_pid(&fx, &first).await;
        signal(pid, rustix::process::Signal::STOP);
        std::fs::write(
            fx.scenario.root().join("runtime/delivery-fault.json"),
            json!({"contains":"写后窗口","action":action}).to_string(),
        )
        .unwrap();
        fx.scenario
            .endpoint()
            .enqueue(fx.main(), ModelReply::text("已收到"));
        let reply = fx
            .command(
                "window-send",
                "session.send",
                json!({"session":session,"text":"写后窗口"}),
            )
            .await;
        assert!(matches!(
            reply,
            CommandReply::Receipt { .. } | CommandReply::DeliveryUnknown
        ));
        if action == "unknown_after_write" {
            fx.wait(&session, "unknown delivery", |s| {
                prompt(s, "写后窗口").is_some_and(|i| i.data["state"] == "unknown")
            })
            .await;
            let refused = fx
                .command(
                    "no-blind-resend",
                    "session.resend",
                    json!({"session":session,"message":"window-send"}),
                )
                .await;
            assert!(matches!(
                refused,
                CommandReply::Receipt {
                    receipt: Receipt::Rejected { .. }
                }
            ));
            fx.scenario.restart_daemon().unwrap();
        }
        // 写出后的故障可晚于 command 收据；一次性 peek 会撞上旧连接关闭。
        // 和产品界面共用重连副本，必须等到新纪元里的真实投递确认。
        let mut feed =
            nd_ui_core::ReplicaFeed::start(fx.socket(), format!("session/{session}")).unwrap();
        signal(pid, rustix::process::Signal::CONT);
        let done = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let update = feed
                    .recv()
                    .await
                    .expect("replica feed closed before recovery");
                if let nd_ui_core::FeedUpdate::Snapshot(snapshot) = update
                    && snapshot.epoch != first.epoch
                    && texts(&snapshot) == ["第一轮", "已收到"]
                    && prompt(&snapshot, "写后窗口").is_some_and(|i| i.data["state"] == "landed")
                {
                    break snapshot;
                }
            }
        })
        .await
        .expect("new daemon epoch must clarify original delivery");
        let mut ui = fx.ui().await;
        assert_eq!(cli_pid(&fx, &done).await, pid);
        assert_eq!(fx.scenario.endpoint().requests().len(), 2);
        assert!(matches!(
            ui.receipt("window-send").await.unwrap(),
            nd_wire::ReceiptLookup::Found { .. }
        ));
        assert!(
            !fx.scenario
                .root()
                .join("runtime/delivery-fault.json")
                .exists(),
            "fault really fired"
        );
        feed.close().await;
        fx.close();
    }
}

#[tokio::test]
async fn missing_input_journal_is_unknown_not_permission_to_write_again() {
    let fx = Fixture::start("nd19-missing-spool", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("第一轮"));
    let session = fx
        .create("missing-create", "/sandbox/project", "开始")
        .await;
    let first = fx.wait(&session, "ready", |s| texts(s) == ["第一轮"]).await;
    let pid = cli_pid(&fx, &first).await;
    signal(pid, rustix::process::Signal::STOP);
    let fault = fx.scenario.root().join("runtime/delivery-fault.json");
    std::fs::write(
        &fault,
        json!({"contains":"流水暂不可读","action":"pause_after_write"}).to_string(),
    )
    .unwrap();
    let mut ui = fx.ui().await;
    let cmd = Command {
        id: "missing-send".into(),
        device: "test".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"流水暂不可读"}),
        expect: json!({}),
    };
    let send = tokio::spawn(async move { ui.command(&cmd).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fault.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let run = header(&first)["process"]["run"].as_str().unwrap();
    let directory = fx.scenario.root().join("runtime/runs").join(run);
    let spool = directory.join("spool");
    let saved = directory.join("spool.saved");
    std::fs::rename(&spool, &saved).unwrap();
    std::fs::create_dir(&spool).unwrap();
    fx.scenario.kill_daemon().unwrap();
    fx.ui().await;
    let unknown = fx
        .wait(&session, "missing evidence stays unknown", |s| {
            prompt(s, "流水暂不可读").is_some_and(|i| i.data["state"] == "unknown")
        })
        .await;
    assert_eq!(cli_pid(&fx, &unknown).await, pid);
    assert_eq!(fx.scenario.endpoint().requests().len(), 1);
    // 恢复真实流水；原管道中的输入仍在，CLI 应只消费一次。
    for entry in std::fs::read_dir(&saved).unwrap() {
        let entry = entry.unwrap();
        std::fs::rename(entry.path(), spool.join(entry.file_name())).unwrap();
    }
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("原消息到了"));
    signal(pid, rustix::process::Signal::CONT);
    fx.wait(&session, "original echo clarifies", |s| {
        texts(s) == ["第一轮", "原消息到了"]
            && prompt(s, "流水暂不可读").is_some_and(|i| i.data["state"] == "landed")
    })
    .await;
    assert_eq!(fx.scenario.endpoint().requests().len(), 2);
    let _ = send.await;
    fx.close();
}

#[tokio::test]
async fn recovery_confirms_an_unwritten_unknown_and_nd_wire_resends_only_on_request() {
    let fx = Fixture::start("nd19-lost", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("就绪"));
    let session = fx.create("lost-create", "/sandbox/project", "开始").await;
    fx.wait(&session, "ready", |s| texts(s) == ["就绪"]).await;
    std::fs::write(
        fx.scenario.root().join("runtime/delivery-fault.json"),
        json!({"contains":"","action":"unknown_without_write"}).to_string(),
    )
    .unwrap();
    let png = include_bytes!("fixtures/pixel.png");
    let blob = fx.ui().await.put_blob(png).await.unwrap();
    let attachments =
        json!([{"blob":blob,"name":"重发.png","media_type":"image/png","size":png.len()}]);
    let original = Command {
        id: "lost-send".into(),
        device: "test".into(),
        name: "session.send".into(),
        args: json!({"session":session,"text":"","attachments":attachments}),
        expect: json!({}),
    };
    let receipt = fx.ui().await.command(&original).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        fx.wait(&session, "unknown", |s| {
            prompt(s, "").is_some_and(|i| i.data["state"] == "unknown")
        }),
    )
    .await
    .unwrap();
    fx.scenario.restart_daemon().unwrap();
    fx.wait(&session, "confirmed absent input", |s| {
        prompt(s, "").is_some_and(|i| i.data["state"] == "not_delivered")
    })
    .await;
    assert_eq!(
        fx.scenario.endpoint().requests().len(),
        1,
        "confirmed loss must not auto resend"
    );
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("重发收到了"));
    // 即使当前草稿恰好与原消息相同，重发也不能把它当成本次发送的草稿清掉。
    let mut draft_command = edit_draft("lost-current-draft", "test", &session, 0, "");
    draft_command.args["attachments"] = attachments.clone();
    fx.ui().await.command(&draft_command).await.unwrap();
    let before_resend = fx.peek(&session).await;
    let resend = Command {
        id: "lost-resend".into(),
        device: "test".into(),
        name: "session.resend".into(),
        args: json!({"session":session,"message":"lost-send"}),
        expect: json!({"draft_version":1}),
    };
    let first = fx.ui().await.command(&resend).await.unwrap();
    assert!(matches!(
        first,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    assert_eq!(fx.ui().await.command(&resend).await.unwrap(), first);
    let done = fx
        .wait(&session, "resend done", |s| {
            texts(s) == ["就绪", "重发收到了"]
        })
        .await;
    assert_eq!(fx.ui().await.command(&original).await.unwrap(), receipt);
    assert_eq!(
        draft(&done),
        draft(&before_resend),
        "resend does not consume the current draft"
    );
    for id in ["lost-send", "lost-resend"] {
        let item = done.items.iter().find(|i| i.data["message"] == id).unwrap();
        assert_eq!(
            item.data["attachments"], attachments,
            "both messages retain their attachments"
        );
    }
    let requests = fx.scenario.endpoint().requests();
    assert_eq!(requests.len(), 2);
    let image = requests[1].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .find(|b| b["type"] == "image")
        .expect("explicit resend must deliver the original image");
    assert_eq!(image["source"]["media_type"], "image/png");
    assert_eq!(
        image["source"]["data"],
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9l8AAAAASUVORK5CYII="
    );
    assert_eq!(fx.ui().await.get_blob(&blob).await.unwrap(), png);
    fx.close();
}

/// #17: 上传成功不是验收；经真 CLI 检查模型请求的图片原字节与消息里的稳定引用。
#[tokio::test]
async fn attachments_reach_the_model_and_remain_in_the_conversation() {
    let fx = Fixture::start("nd17-image", 3_600_000).await;
    let png: &[u8] = include_bytes!("fixtures/pixel.png");
    let ui = fx.ui().await;
    let blob = ui.put_blob(png).await.unwrap();
    assert_eq!(ui.put_blob(png).await.unwrap(), blob);
    let attachments =
        json!([{"blob":blob,"name":"像素.png","media_type":"image/png","size":png.len()}]);
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("看到图片了"));
    let reply = fx
        .command(
            "image-first",
            "session.create",
            json!({
                "cwd":"/sandbox/home", "model":MODEL, "text":"看图", "attachments":attachments
            }),
        )
        .await;
    let session = match reply {
        CommandReply::Receipt {
            receipt:
                Receipt::Accepted {
                    stream: Some(stream),
                    ..
                },
        } => stream.trim_start_matches("session/").to_owned(),
        other => panic!("{other:?}"),
    };
    let snapshot = fx
        .wait(&session, "image response", |s| texts(s) == ["看到图片了"])
        .await;
    let requests = fx.scenario.endpoint().requests();
    let image = requests[0].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .find(|b| b["type"] == "image")
        .expect("model must receive an image block");
    assert_eq!(image["source"]["media_type"], "image/png");
    assert_eq!(
        image["source"]["data"],
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9l8AAAAASUVORK5CYII="
    );
    assert_eq!(
        prompt(&snapshot, "看图").unwrap().data["attachments"],
        attachments
    );
    assert_eq!(ui.get_blob(&blob).await.unwrap(), png);
    fx.close();
}

#[tokio::test]
async fn attachment_validation_rejects_a_whole_message_without_holding_partial_uploads() {
    let fx = Fixture::start("nd17-invalid", 3_600_000).await;
    let ui = fx.ui().await;
    let bytes = include_bytes!("fixtures/pixel.png");
    let blob = ui.put_blob(bytes).await.unwrap();
    let good = json!({"blob":blob,"name":"image.png","media_type":"image/png","size":bytes.len()});
    for (index, attachments) in [
        json!([good, {"blob":"0".repeat(64),"name":"missing.png","media_type":"image/png","size":1}]),
        { let mut a = good.clone(); a["size"] = json!(999); json!([a]) },
        { let mut a = good.clone(); a["media_type"] = json!("application/octet-stream"); json!([a]) },
    ].into_iter().enumerate() {
        let reply = fx.command(&format!("invalid-{index}"), "session.create", json!({"cwd":"/sandbox/home","model":MODEL,"text":"拒绝附件","attachments":attachments})).await;
        assert!(matches!(reply, CommandReply::Receipt { receipt: Receipt::Rejected { ref code, .. } } if code == "invalid_attachment"), "{reply:?}");
    }
    assert!(fx.scenario.endpoint().requests().is_empty());
    // 经守护进程的正常清理器观察：整条拒绝的命令不能留下部分附件引用。
    let config = fx.scenario.root().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[storage]\nblob_grace_seconds=0\ngc_interval_seconds=1\n");
    std::fs::write(config, text).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while ui.get_blob(&blob).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("rejected upload must be collected");
    fx.close();
}

#[tokio::test]
async fn files_without_caption_reach_the_model_and_survive_restart_and_collection() {
    let fx = Fixture::start("nd17-files", 3_600_000).await;
    let ui = fx.ui().await;
    let text = "文件正文：中文、emoji 🦀\n第二行\n";
    let text_blob = ui.put_blob(text.as_bytes()).await.unwrap();
    let pdf = include_bytes!("fixtures/material.pdf");
    let pdf_blob = ui.put_blob(pdf).await.unwrap();
    let attachments = json!([
        {"blob":text_blob,"name":"材料.md","media_type":"text/plain","size":text.len()},
        {"blob":pdf_blob,"name":"material.pdf","media_type":"application/pdf","size":pdf.len()}
    ]);
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("已读材料"));
    let reply = fx
        .command(
            "files-first",
            "session.create",
            json!({"cwd":"/sandbox/home","model":MODEL,"text":"","attachments":attachments}),
        )
        .await;
    let session = match reply {
        CommandReply::Receipt {
            receipt:
                Receipt::Accepted {
                    stream: Some(stream),
                    ..
                },
        } => stream.trim_start_matches("session/").to_owned(),
        other => panic!("{other:?}"),
    };
    fx.wait(&session, "file response", |s| texts(s) == ["已读材料"])
        .await;
    let requests = fx.scenario.endpoint().requests();
    let blocks: Vec<_> = requests[0].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .collect();
    assert!(
        blocks
            .iter()
            .any(|b| b["text"].as_str().is_some_and(|s| s.contains(text)))
    );
    let doc = blocks
        .iter()
        .find(|b| b["type"] == "document")
        .expect("PDF reaches model as document");
    assert_eq!(doc["source"]["media_type"], "application/pdf");
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(doc["source"]["data"].as_str().unwrap())
            .unwrap(),
        pdf
    );
    let config = fx.scenario.root().join("config.toml");
    let mut config_text = std::fs::read_to_string(&config).unwrap();
    config_text.push_str("\n[storage]\nblob_grace_seconds=0\ngc_interval_seconds=1\n");
    std::fs::write(config, config_text).unwrap();
    fx.scenario.restart_daemon().unwrap();
    let ui = fx.ui().await;
    let orphan = ui.put_blob(b"unreferenced").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while ui.get_blob(&orphan).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ui.get_blob(&text_blob).await.unwrap(), text.as_bytes());
    assert_eq!(ui.get_blob(&pdf_blob).await.unwrap(), pdf);
    assert_eq!(
        prompt(&fx.peek(&session).await, "").unwrap().data["attachments"],
        attachments
    );
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("第二份"));
    let reply = fx
        .command(
            "files-send",
            "session.send",
            json!({"session":session,"text":"","attachments":attachments}),
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
    fx.wait(&session, "second file response", |s| {
        texts(s) == ["已读材料", "第二份"]
    })
    .await;
    assert!(request_text(&fx.scenario.endpoint().requests()[1].body).contains("文件正文"));
    fx.close();
}

#[tokio::test]
async fn desktop_upload_reads_file_bytes_and_rejects_unsupported_or_missing_files() {
    use nd_ui_core::{AttachmentSource, CommandClient};
    let fx = Fixture::start("nd17-upload", 3_600_000).await;
    let client = CommandClient::start(fx.socket()).unwrap();
    let path = fx.scenario.root().join("材料.md");
    std::fs::write(&path, "中文材料").unwrap();
    let a = client
        .upload(AttachmentSource::Path(path.clone()))
        .await
        .unwrap();
    assert_eq!(a.name, "材料.md");
    assert_eq!(a.media_type, "text/plain");
    assert_eq!(
        fx.ui().await.get_blob(&a.blob).await.unwrap(),
        "中文材料".as_bytes()
    );
    assert!(
        client
            .upload(AttachmentSource::Bytes {
                name: "data.bin".into(),
                bytes: vec![0, 255, 1]
            })
            .await
            .unwrap_err()
            .contains("不支持")
    );
    assert!(
        client
            .upload(AttachmentSource::Path(fx.scenario.root().into()))
            .await
            .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    assert!(client.upload(AttachmentSource::Path(path)).await.is_err());
    fx.close();
}

#[tokio::test]
async fn native_attachment_paste_drop_and_diff_rendering() {
    let fx = Fixture::start("nd17-window", 3_600_000).await;
    let answer = "说明\n```rust\nfn main() {}\n```\n```diff\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-旧内容\n+新内容🦀\n```\n结束";
    fx.scenario.endpoint().enqueue(
        Route::new(None, "claude-haiku-4-5-20251001"),
        ModelReply::streaming_text(answer, 1, 75),
    );
    let output = std::env::var_os("ND17_NATIVE_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| fx.scenario.root().join("native-attachments"));
    let result = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
        .arg("--desktop")
        .arg(std::env::var_os("ND_TEST_DESKTOP").unwrap())
        .arg("--socket")
        .arg(fx.socket())
        .arg("--output")
        .arg(&output)
        .arg("--attachments")
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let requests = fx.scenario.endpoint().requests();
    assert_eq!(requests.len(), 1);
    let blocks: Vec<_> = requests[0].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .collect();
    assert!(
        blocks.iter().any(|b| b["type"] == "image"),
        "pasted image did not reach the model"
    );
    let text = request_text(&requests[0].body);
    assert!(
        text.contains("复制文件里的中文正文") && text.contains("拖入文件里的独立正文"),
        "{text}"
    );
    let verdict: Value =
        serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap();
    let snapshot = fx.peek(verdict["session"].as_str().unwrap()).await;
    assert_eq!(
        prompt(&snapshot, "请写代码").unwrap().data["attachments"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    fx.close();
}

#[tokio::test]
async fn a_multi_megabyte_attachment_is_not_lost_at_the_watchdog_frame_boundary() {
    let fx = Fixture::start("nd17-large", 3_600_000).await;
    let bytes = "材料".repeat(400_000);
    let blob = fx.ui().await.put_blob(bytes.as_bytes()).await.unwrap();
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("大文件已收到"));
    let reply = fx.command("large-create", "session.create", json!({"cwd":"/sandbox/home","model":MODEL,"text":"大附件","attachments":[{"blob":blob,"name":"large.txt","size":bytes.len(),"media_type":"text/plain"}]})).await;
    let CommandReply::Receipt {
        receipt: Receipt::Accepted {
            stream: Some(stream),
            ..
        },
    } = reply
    else {
        panic!("{reply:?}");
    };
    let session = stream.trim_start_matches("session/");
    let snapshot = fx
        .wait(session, "large reply or explicit failure", |s| {
            texts(s) == ["大文件已收到"] || has_header(s, |h| h["status"] == "partial")
        })
        .await;
    assert_eq!(texts(&snapshot), ["大文件已收到"]);
    let requests = fx.scenario.endpoint().requests();
    let received = requests[0].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter_map(|b| b["text"].as_str())
        .any(|t| t.contains(&bytes));
    assert!(
        received,
        "large attachment contents must reach the endpoint intact"
    );
    fx.close();
}

#[tokio::test]
async fn withdrawn_creation_releases_unused_attachments_and_keeps_its_receipt() {
    let fx = Fixture::start("nd17-withdraw", 3_600_000).await;
    let ui = fx.ui().await;
    let blob = ui.put_blob(b"unused material").await.unwrap();
    let args = json!({"cwd":"/sandbox/missing","text":"无法开始","attachments":[{"blob":blob,"name":"unused.txt","size":15,"media_type":"text/plain"}]});
    let reply = fx
        .command("withdraw-create", "session.create", args.clone())
        .await;
    let CommandReply::Receipt {
        receipt: Receipt::Accepted {
            stream: Some(ref stream),
            ..
        },
    } = reply
    else {
        panic!("{reply:?}");
    };
    fx.wait(stream.trim_start_matches("session/"), "withdrawn", |s| {
        has_header(s, |h| h["status"] == "withdrawn")
    })
    .await;
    let config = fx.scenario.root().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[storage]\nblob_grace_seconds=0\ngc_interval_seconds=1\n");
    std::fs::write(config, text).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while ui.get_blob(&blob).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("withdrawn creation without a prompt releases attachment");
    assert_eq!(
        fx.command("withdraw-create", "session.create", args).await,
        reply
    );
    assert!(fx.scenario.endpoint().requests().is_empty());
    fx.close();
}

#[tokio::test]
async fn jpeg_gif_and_webp_attachments_reach_the_model_with_their_original_bytes() {
    use base64::Engine;
    let fx = Fixture::start("nd17-formats", 3_600_000).await;
    let ui = fx.ui().await;
    let samples: [(&str, &[u8]); 3] = [
        ("image/jpeg", include_bytes!("fixtures/preview.jpg")),
        ("image/gif", include_bytes!("fixtures/preview.gif")),
        ("image/webp", include_bytes!("fixtures/preview.webp")),
    ];
    let mut attachments = vec![];
    for (mime, bytes) in samples {
        attachments.push(json!({"blob":ui.put_blob(bytes).await.unwrap(),"name":mime,"media_type":mime,"size":bytes.len()}));
    }
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("三种图片"));
    let reply=fx.command("formats-create","session.create",json!({"cwd":"/sandbox/home","model":MODEL,"text":"核对格式","attachments":attachments})).await;
    let CommandReply::Receipt {
        receipt: Receipt::Accepted {
            stream: Some(stream),
            ..
        },
    } = reply
    else {
        panic!("{reply:?}");
    };
    fx.wait(
        stream.trim_start_matches("session/"),
        "formats reply",
        |s| texts(s) == ["三种图片"],
    )
    .await;
    let requests = fx.scenario.endpoint().requests();
    let images: Vec<_> = requests[0].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "image")
        .collect();
    assert_eq!(images.len(), 3);
    for ((mime, bytes), image) in samples.into_iter().zip(images) {
        assert_eq!(image["source"]["media_type"], mime);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(image["source"]["data"].as_str().unwrap())
                .unwrap(),
            bytes
        );
    }
    fx.close();
}

#[tokio::test]
async fn real_edit_tool_exposes_the_replaced_text_for_diff_display() {
    let fx = Fixture::start("nd17-edit", 3_600_000).await;
    std::fs::write(
        fx.scenario.root().join("claude/settings.json"),
        r#"{"permissions":{"allow":["Read","Edit"]}}"#,
    )
    .unwrap();
    let file = fx.scenario.root().join("project/example.txt");
    std::fs::write(&file, "旧内容\n").unwrap();
    let input = json!({"file_path":"/sandbox/project/example.txt","old_string":"旧内容\n","new_string":"新内容🦀\n"});
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(
        fx.main(),
        ModelReply::tool(
            "toolu_read_diff",
            "Read",
            json!({"file_path":"/sandbox/project/example.txt"}),
        ),
    );
    endpoint.enqueue(
        fx.main(),
        ModelReply::tool("toolu_edit_diff", "Edit", input.clone()),
    );
    endpoint.enqueue(fx.main(), ModelReply::text("改好了"));
    let session = fx
        .create("edit-diff-create", "/sandbox/project", "修改测试文件")
        .await;
    let snapshot = fx
        .wait(&session, "Edit finished", |s| texts(s) == ["改好了"])
        .await;
    let edit = snapshot
        .items
        .iter()
        .find(|i| i.kind == "tool_use" && i.data["raw"]["name"] == "Edit")
        .expect("Edit available to diff projection");
    for key in ["file_path", "old_string", "new_string"] {
        assert_eq!(edit.data["raw"]["input"][key], input[key]);
    }
    assert_eq!(std::fs::read_to_string(file).unwrap(), "新内容🦀\n");
    fx.close();
}

#[tokio::test]
async fn draft_attachments_survive_conflicts_restart_and_transfer_to_the_sent_message() {
    let fx = Fixture::start("nd17-draft", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("开始"));
    let session = fx
        .create("attached-draft-create", "/sandbox/project", "开始")
        .await;
    fx.wait(&session, "active", |s| texts(s) == ["开始"]).await;
    let ui = fx.ui().await;
    let blob = ui.put_blob(b"draft attachment").await.unwrap();
    let attachments = json!([{"blob":blob,"name":"draft.txt","media_type":"text/plain","size":16}]);
    let command = |id: &str, text: &str, version: u64| Command {
        id: id.into(),
        device: id.into(),
        name: "session.draft.update".into(),
        args: json!({"session":session,"text":text,"attachments":attachments}),
        expect: json!({"draft_version":version}),
    };
    let first = command("attached-a", "A", 0);
    let _ = fx.ui().await.command(&first).await.unwrap();
    let _ = fx
        .ui()
        .await
        .command(&command("attached-b", "B", 0))
        .await
        .unwrap();
    fx.scenario.restart_daemon().unwrap();
    let snapshot = fx.peek(&session).await;
    let draft = &snapshot
        .items
        .iter()
        .find(|i| i.id == "draft")
        .unwrap()
        .data;
    assert_eq!(draft["attachments"], attachments);
    assert_eq!(draft["saved"][0]["attachments"], attachments);
    let config = fx.scenario.root().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("\n[storage]\nblob_grace_seconds=0\ngc_interval_seconds=1\n");
    std::fs::write(config, text).unwrap();
    let ui = fx.ui().await;
    let orphan = ui.put_blob(b"no reference").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while ui.get_blob(&orphan).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ui.get_blob(&blob).await.unwrap(), b"draft attachment");
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("收到草稿附件"));
    let reply = fx
        .ui()
        .await
        .command(&Command {
            id: "attached-draft-send".into(),
            device: "a".into(),
            name: "session.send".into(),
            args: json!({"session":session,"text":"A","attachments":attachments}),
            expect: json!({"draft_version":1}),
        })
        .await
        .unwrap();
    assert!(matches!(
        reply,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    let done = fx
        .wait(&session, "attached draft sent", |s| {
            texts(s) == ["开始", "收到草稿附件"]
        })
        .await;
    let draft = &done.items.iter().find(|i| i.id == "draft").unwrap().data;
    assert_eq!(draft["attachments"], json!([]));
    assert_eq!(draft["saved"][0]["attachments"], attachments);
    assert_eq!(prompt(&done, "A").unwrap().data["attachments"], attachments);
    fx.close();
}

#[tokio::test]
async fn settings_model_changes_the_next_turn_and_survives_restart() {
    let fx = Fixture::start("nd21-model", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| {
        header(s)["status"] == "active" && header(s)["process"]["turn_running"] == false
    })
    .await;
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text("first"));
    fx.send("busy", &session, "more").await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    fx.wait(&session, "running", |s| {
        header(s)["process"]["turn_running"] == true
    })
    .await;
    for (id, setting, key, value) in [(
        "mid-turn-permission",
        json!({"permission_mode":"acceptEdits"}),
        "permission_mode",
        "acceptEdits",
    )] {
        fx.command(
            id,
            "session.configure",
            json!({"session":session,"setting":setting}),
        )
        .await;
        let changed = tokio::time::timeout(
            Duration::from_secs(3),
            fx.wait(&session, "mid-turn setting", |s| {
                header(s)["op"].is_null()
                    && if key == "permission_mode" {
                        header(s)["permission_mode"] == value
                    } else {
                        header(s)["settings"]["applied"][key] == value
                    }
            }),
        )
        .await;
        let changed = match changed {
            Ok(changed) => changed,
            Err(_) => panic!(
                "{id} did not apply mid-turn: {}",
                header(&fx.peek(&session).await)
            ),
        };
        assert_eq!(header(&changed)["process"]["turn_running"], true);
    }
    let reply = fx
        .command(
            "model",
            "session.configure",
            json!({"session":session,"setting":{"model":"claude-sonnet-4-6"}}),
        )
        .await;
    assert!(
        matches!(
            reply,
            CommandReply::Receipt {
                receipt: Receipt::Accepted { .. }
            }
        ),
        "{reply:?}"
    );
    let next = Route::new(None, "claude-sonnet-4-6");
    endpoint.enqueue(next.clone(), ModelReply::text("valid")); // CLI validates a custom model with max_tokens=1.
    endpoint.enqueue(next.clone(), ModelReply::text("second"));
    fx.send("next", &session, "continue").await;
    assert_eq!(endpoint.requests().len(), 2);
    gate.release();
    endpoint
        .wait_for_requests(&next, 2, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("{e}: requests={:?}", endpoint.requests()));
    assert_eq!(endpoint.requests()[2].body["max_tokens"], 1);
    assert!(request_text(&endpoint.requests()[3].body).contains("continue"));
    let snapshot = fx
        .wait(&session, "second turn", |s| {
            texts(s).iter().any(|t| t == "second")
        })
        .await;
    assert_eq!(header(&snapshot)["model"], "claude-sonnet-4-6");
    fx.scenario.kill_daemon().unwrap();
    let after = fx
        .wait(&session, "reopened", |s| {
            header(s)["model"] == "claude-sonnet-4-6"
        })
        .await;
    assert_eq!(
        header(&after)["settings"]["applied"]["model"],
        "claude-sonnet-4-6"
    );
}

#[tokio::test]
async fn settings_effort_and_ultracode_follow_cli_availability_and_preserve_effort() {
    let fx = Fixture::start("nd21-flags", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| {
        header(s)["status"] == "active" && header(s)["process"]["turn_running"] == false
    })
    .await;
    let mut busy_gate = None;
    for (id, setting, key, expected) in [
        (
            "opus",
            json!({"model":"opus"}),
            "model",
            json!("claude-opus-5-5"),
        ),
        ("effort", json!({"effort":"high"}), "effort", json!("high")),
        ("on", json!({"ultracode":true}), "ultracode", json!(true)),
        ("off", json!({"ultracode":false}), "ultracode", json!(false)),
    ] {
        let reply = fx
            .command(
                id,
                "session.configure",
                json!({"session":session,"setting":setting}),
            )
            .await;
        assert!(
            matches!(
                reply,
                CommandReply::Receipt {
                    receipt: Receipt::Accepted { .. }
                }
            ),
            "{id}: {reply:?}"
        );
        let s = tokio::time::timeout(
            Duration::from_secs(3),
            fx.wait(&session, id, |s| header(s)["op"].is_null()),
        )
        .await
        .expect("effort and ultracode must apply during the active round");
        if id == "opus" {
            let route = Route::new(None, "claude-opus-5-5");
            busy_gate = Some(
                fx.scenario
                    .endpoint()
                    .enqueue_held(route.clone(), ModelReply::text("finished")),
            );
            fx.send("busy-opus", &session, "keep running").await;
            fx.scenario
                .endpoint()
                .wait_for_requests(&route, 1, Duration::from_secs(20))
                .await
                .unwrap();
            fx.wait(&session, "busy opus", |s| {
                header(s)["process"]["turn_running"] == true
            })
            .await;
        } else {
            assert_eq!(header(&s)["process"]["turn_running"], true);
        }
        assert_eq!(
            header(&s)["settings"]["applied"][key],
            expected,
            "{id}: {}",
            header(&s)
        );
        if id == "on" || id == "off" {
            assert_eq!(header(&s)["settings"]["applied"]["effort"], "high");
            assert_eq!(header(&s)["caps"]["ultracode"], true);
            assert_eq!(
                header(&s)["settings"]["applied"]["ultracode_requested"],
                expected
            );
        }
    }
    busy_gate.unwrap().release();
}

#[tokio::test]
async fn settings_are_read_on_open_and_permissions_and_effort_survive_reclaim() {
    let fx = Fixture::start("nd21-resume-settings", 900).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    let first = fx
        .wait(&session, "active", |s| {
            header(s)["status"] == "active" && header(s)["op"].is_null()
        })
        .await;
    assert_eq!(header(&first)["caps"]["model"], true);
    assert_eq!(header(&first)["caps"]["ultracode"], false);
    assert!(
        !header(&first)["settings"]["permission_modes"]
            .as_array()
            .unwrap()
            .contains(&json!("bypassPermissions")),
        "a process without launch authorization must not offer bypassPermissions"
    );
    let denied = fx
        .command(
            "ultra-denied",
            "session.configure",
            json!({"session":session,"setting":{"ultracode":true}}),
        )
        .await;
    assert!(matches!(
        denied,
        CommandReply::Receipt {
            receipt: Receipt::Rejected { .. }
        }
    ));
    // Keep a watcher while editing; reclaim only after the watcher closes.
    let mut watcher = fx.ui().await;
    watcher
        .subscribe(&format!("session/{session}"))
        .await
        .unwrap();
    for (id, setting) in [
        ("mode", json!({"permission_mode":"acceptEdits"})),
        ("opus", json!({"model":"opus"})),
        ("effort", json!({"effort":"high"})),
    ] {
        let reply = fx
            .command(
                id,
                "session.configure",
                json!({"session":session,"setting":setting}),
            )
            .await;
        assert!(
            matches!(
                reply,
                CommandReply::Receipt {
                    receipt: Receipt::Accepted { .. }
                }
            ),
            "{reply:?}"
        );
        fx.wait(&session, id, |s| header(s)["op"].is_null()).await;
    }
    watcher.close().await.unwrap();
    fx.wait(&session, "reclaimed", |s| {
        header(s)["process"]["alive"] == false && header(s)["op"].is_null()
    })
    .await;
    let opus = Route::new(None, "claude-opus-5-5");
    fx.scenario
        .endpoint()
        .enqueue(opus.clone(), ModelReply::text("resumed"));
    fx.send("resume", &session, "continue").await;
    fx.scenario
        .endpoint()
        .wait_for_requests(&opus, 1, Duration::from_secs(30))
        .await
        .unwrap();
    let snapshot = fx
        .wait(&session, "resumed", |s| {
            texts(s).iter().any(|s| s == "resumed")
        })
        .await;
    assert_eq!(header(&snapshot)["permission_mode"], "acceptEdits");
    assert_eq!(header(&snapshot)["settings"]["applied"]["effort"], "high");
    assert_eq!(
        fx.scenario.endpoint().requests().last().unwrap().body["output_config"]["effort"],
        "high"
    );
}

#[tokio::test]
async fn settings_manual_title_updates_sidebar_and_survives_resume() {
    let fx = Fixture::start("nd21-rename", 900).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| {
        header(s)["status"] == "active" && header(s)["op"].is_null()
    })
    .await;
    let reply = fx
        .command(
            "rename",
            "session.rename",
            json!({"session":session,"title":"新的中文标题 🦀"}),
        )
        .await;
    assert!(
        matches!(
            reply,
            CommandReply::Receipt {
                receipt: Receipt::Accepted { .. }
            }
        ),
        "{reply:?}"
    );
    fx.wait(&session, "renamed", |s| {
        header(s)["title"] == "新的中文标题 🦀" && header(s)["op"].is_null()
    })
    .await;
    let global = fx.ui().await.subscribe("global").await.unwrap();
    assert!(
        global
            .items
            .iter()
            .any(|i| i.data["title"] == "新的中文标题 🦀")
    );
    fx.scenario.kill_daemon().unwrap();
    fx.wait(&session, "reclaimed", |s| {
        header(s)["process"]["alive"] == false && header(s)["op"].is_null()
    })
    .await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("resumed"));
    fx.send("resume", &session, "continue").await;
    let after = fx
        .wait(&session, "resumed", |s| {
            texts(s).iter().any(|s| s == "resumed")
        })
        .await;
    assert_eq!(header(&after)["title"], "新的中文标题 🦀");
    assert_eq!(fx.scenario.endpoint().requests().len(), 2);
    let native = fx.transcript(
        header(&after)["process"]["backend_session"]
            .as_str()
            .unwrap(),
    );
    assert!(
        native
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|row| row["type"] == "custom-title" && row["customTitle"] == "新的中文标题 🦀")
    );
}

#[tokio::test]
async fn settings_ai_title_is_generated_once_and_return_value_updates_the_sidebar() {
    let fx = Fixture::start("nd21-title-ai", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text(r#"{"title":"会话设置实现"}"#));
    let session = fx
        .create("create", "/sandbox/project", "请实现会话设置与标题管理功能")
        .await;
    let s = fx
        .wait(&session, "AI title", |s| header(s)["title_source"] == "ai")
        .await;
    assert_eq!(header(&s)["title"], "会话设置实现");
    fx.scenario.kill_daemon().unwrap();
    assert_eq!(header(&fx.peek(&session).await)["title"], "会话设置实现");
    assert_eq!(fx.scenario.endpoint().requests().len(), 2);
    let text = fx.transcript(header(&s)["process"]["backend_session"].as_str().unwrap());
    assert!(text.contains("ai-title") && text.contains("会话设置实现"));
}

#[tokio::test]
async fn settings_inflight_ai_title_survives_restart_and_never_overwrites_manual_title() {
    let fx = Fixture::start("nd21-title-ai-restart", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let gate = endpoint.enqueue_held(fx.main(), ModelReply::text(r#"{"title":"晚到的自动标题"}"#));
    let session = fx
        .create("create", "/sandbox/project", "为会话设置增加持久化和标题")
        .await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    fx.scenario.kill_daemon().unwrap();
    fx.peek(&session).await;
    let reply = fx
        .command(
            "rename",
            "session.rename",
            json!({"session":session,"title":"我的标题"}),
        )
        .await;
    assert!(
        matches!(
            reply,
            CommandReply::Receipt {
                receipt: Receipt::Accepted { .. }
            }
        ),
        "{reply:?}"
    );
    fx.wait(&session, "manual title", |s| {
        header(s)["title"] == "我的标题"
    })
    .await;
    gate.release();
    let after = fx
        .wait(&session, "all title operations complete", |s| {
            header(s)["op"].is_null()
        })
        .await;
    assert_eq!(header(&after)["title"], "我的标题");
    assert_eq!(
        endpoint.requests().len(),
        2,
        "recovery must not repeat AI generation"
    );
}

#[tokio::test]
async fn history_pages_are_read_only_ordered_and_resume_after_restart() {
    let fx = Fixture::start("nd20-history", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("第一页回答"));
    let session = fx
        .create("history-create", "/sandbox/project", "第一页提示")
        .await;
    fx.wait(&session, "first answer", |s| {
        texts(s).contains(&"第一页回答".into())
    })
    .await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("第二页回答"));
    fx.send("history-send", &session, "第二页提示").await;
    fx.wait(&session, "second answer", |s| {
        texts(s).contains(&"第二页回答".into())
    })
    .await;
    let mut ui = fx.ui().await;
    let res = format!("session/{session}");
    let mut req = nd_wire::PageReq {
        limit: 2,
        ..Default::default()
    };
    let mut pages = Vec::new();
    loop {
        let page = ui
            .get(&res, req.clone())
            .await
            .expect("history page via nd-wire");
        assert!(page.items.len() <= 2);
        assert!(
            page.items
                .windows(2)
                .all(|w| w[0].data["seq"].as_u64() < w[1].data["seq"].as_u64())
        );
        pages.splice(0..0, page.items);
        req.before = page.next;
        if req.before.is_none() {
            break;
        }
    }
    let ids: std::collections::BTreeSet<_> = pages.iter().map(|i| &i.id).collect();
    assert_eq!(ids.len(), pages.len(), "no boundary duplicates");
    let prompts: Vec<_> = pages
        .iter()
        .filter(|i| i.kind == "prompt")
        .map(|i| i.data["text"].as_str().unwrap())
        .collect();
    assert_eq!(prompts, ["第一页提示", "第二页提示"]);
    assert_eq!(
        fx.scenario.endpoint().requests().len(),
        2,
        "reading must not call the model"
    );
    fx.scenario.kill_daemon().unwrap();
    fx.scenario.restart_daemon().unwrap();
    let mut cold = fx.ui().await;
    let mut req = nd_wire::PageReq {
        limit: 100,
        ..Default::default()
    };
    let restored = cold.get(&res, req.clone()).await.unwrap();
    assert_eq!(
        restored.items.iter().filter(|i| i.kind == "prompt").count(),
        2
    );
    req.before = Some("bad-cursor".into());
    assert!(cold.get(&res, req).await.is_err());
    fx.close();
}

#[tokio::test]
async fn a_thousand_real_rounds_open_bounded_and_jump_through_public_pages() {
    let fx = Fixture::start("nd20-thousand", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ANSWER 1"));
    let session = fx
        .create("scale-create", "/sandbox/project", "PROMPT 1")
        .await;
    for n in 1..=1000 {
        if n > 1 {
            fx.scenario
                .endpoint()
                .enqueue(fx.main(), ModelReply::text(format!("ANSWER {n}")));
            fx.send(&format!("scale-{n}"), &session, &format!("PROMPT {n}"))
                .await;
        }
        fx.wait(&session, "indexed complete round", |s| {
            s.items.iter().any(|i| {
                i.kind == "lineage"
                    && i.data["rounds"]
                        .as_array()
                        .is_some_and(|r| r.len() == n && r[n - 1]["complete"] == true)
            })
        })
        .await;
        if n % 100 == 0 {
            eprintln!("history-scale: {n}/1000");
        }
    }
    let start = std::time::Instant::now();
    let snapshot = fx.peek(&session).await;
    eprintln!(
        "history-scale: cold snapshot {:?}, {} bytes",
        start.elapsed(),
        serde_json::to_vec(&snapshot).unwrap().len()
    );
    assert!(
        snapshot
            .items
            .iter()
            .filter(|i| !matches!(
                i.kind.as_str(),
                "header" | "draft" | "lineage" | "navigation" | "history"
            ))
            .count()
            <= 60
    );
    let nav = snapshot
        .items
        .iter()
        .find(|i| i.kind == "navigation")
        .unwrap();
    assert_eq!(nav.data["rounds"].as_array().unwrap().len(), 1000);
    let client = nd_ui_core::CommandClient::start(fx.socket()).unwrap();
    for n in [1, 500, 1000] {
        let round = nav.data["rounds"][n - 1]["id"].as_str().unwrap();
        let page = client
            .get(
                format!("session/{session}/items"),
                nd_wire::PageReq {
                    around: Some(round.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items[0].data["text"], format!("PROMPT {n}"));
        assert_eq!(page.anchor.as_deref(), Some(page.items[0].id.as_str()));
    }
    native_history(
        &fx,
        &session,
        nav.data["rounds"][499]["id"].as_str().unwrap(),
        "PROMPT 500",
        1000,
    )
    .await;
    if let Some(path) = std::env::var_os("ND_HISTORY_REVIEW_FILE") {
        let path = std::path::PathBuf::from(path);
        std::fs::write(&path, serde_json::to_vec_pretty(&json!({"socket":fx.socket(),"session":session,"round":nav.data["rounds"][499]["id"],"desktop":std::env::var_os("ND_TEST_DESKTOP")})).unwrap()).unwrap();
        eprintln!(
            "owner review ready: {}; remove this file to clean up",
            path.display()
        );
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    assert_eq!(fx.scenario.endpoint().requests().len(), 1000);
    fx.close();
}

#[tokio::test]
async fn native_navigation_jumps_to_a_round_via_history_page() {
    let fx = Fixture::start("nd20-native", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("原生导航回答"));
    let session = fx
        .create("native-nav-create", "/sandbox/project", "原生导航提示")
        .await;
    let done = fx
        .wait(&session, "round indexed", |s| {
            s.items
                .iter()
                .any(|i| i.kind == "navigation" && i.data["rounds"][0]["complete"] == true)
        })
        .await;
    let nav = done.items.iter().find(|i| i.kind == "navigation").unwrap();
    native_history(
        &fx,
        &session,
        nav.data["rounds"][0]["id"].as_str().unwrap(),
        "原生导航提示",
        1,
    )
    .await;
    assert_eq!(fx.scenario.endpoint().requests().len(), 1);
    fx.close();
}

async fn native_history(fx: &Fixture, session: &str, round: &str, text: &str, rounds: usize) {
    let output = std::env::var_os("ND20_NATIVE_OUTPUT")
        .map(|p| std::path::PathBuf::from(p).join(format!("rounds-{rounds}")))
        .unwrap_or_else(|| fx.scenario.root().join("native-history"));
    let result = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
        .arg("--desktop")
        .arg(std::env::var_os("ND_TEST_DESKTOP").unwrap())
        .arg("--socket")
        .arg(fx.socket())
        .arg("--output")
        .arg(output)
        .arg("--session")
        .arg(session)
        .arg("--history")
        .arg("--themes")
        .arg("--round")
        .arg(round)
        .arg("--text")
        .arg(text)
        .arg("--rounds")
        .arg(rounds.to_string())
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn settings_native_window_changes_model_effort_and_title_through_nd_wire() {
    let fx = Fixture::start("nd21-native", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    let output = std::env::var("ND_NATIVE_SETTINGS_OUTPUT").unwrap_or_else(|_| {
        fx.scenario
            .root()
            .join("native-settings")
            .to_string_lossy()
            .into_owned()
    });
    let result = tokio::process::Command::new("python")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../nd-desktop/tests/native_chat.py"))
        .args([
            "--desktop",
            &std::env::var("ND_TEST_DESKTOP").unwrap(),
            "--socket",
            fx.socket().to_str().unwrap(),
            "--output",
            &output,
            "--session",
            &session,
            "--settings",
            "--themes",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fx.scenario.endpoint().requests().len(), 1);
}

#[tokio::test]
async fn settings_stale_clients_conflict_and_cli_rejection_preserves_applied_values() {
    let fx = Fixture::start("nd21-setting-conflict", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    let make = |id: &str, revision: u64, setting: Value| Command {
        id: id.into(),
        device: "settings-ui".into(),
        name: "session.configure".into(),
        args: json!({"session":session,"setting":setting}),
        expect: json!({"settings_revision":revision}),
    };
    let mut ui = fx.ui().await;
    let first = make("mode", 0, json!({"permission_mode":"acceptEdits"}));
    let receipt = ui.command(&first).await.unwrap();
    fx.wait(&session, "mode", |s| header(s)["op"].is_null())
        .await;
    let stale = ui
        .command(&make("stale", 0, json!({"permission_mode":"plan"})))
        .await
        .unwrap();
    assert!(
        matches!(stale,CommandReply::Receipt {receipt:Receipt::Rejected {ref code,..}} if code == "conflict"),
        "{stale:?}"
    );
    assert_eq!(ui.command(&first).await.unwrap(), receipt);
    let invalid = make("invalid-mode", 1, json!({"permission_mode":"not-a-mode"}));
    assert!(matches!(
        ui.command(&invalid).await.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Accepted { .. }
        }
    ));
    let after = fx
        .wait(&session, "rejected", |s| header(s)["op"].is_null())
        .await;
    assert_eq!(header(&after)["permission_mode"], "acceptEdits");
    assert!(
        after
            .items
            .iter()
            .any(|i| i.kind == "op" && i.data["phase"] == "compensated")
    );
}

#[tokio::test]
async fn settings_lost_title_process_settles_without_blocking_the_session() {
    let fx = Fixture::start("nd21-title-ai-exit", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let _gate = endpoint.enqueue_held(fx.main(), ModelReply::text(r#"{"title":"不会返回"}"#));
    let session = fx
        .create("create", "/sandbox/project", "后端退出时保留可用的首条摘要")
        .await;
    endpoint
        .wait_for_requests(&fx.main(), 2, Duration::from_secs(30))
        .await
        .unwrap();
    let s = fx.peek(&session).await;
    signal(cli_pid(&fx, &s).await, rustix::process::Signal::KILL);
    let after = fx
        .wait(&session, "title settled after exit", |s| {
            header(s)["process"]["alive"] == false && header(s)["op"].is_null()
        })
        .await;
    assert_eq!(header(&after)["title_source"], "summary");
    assert!(
        after
            .items
            .iter()
            .any(|i| i.kind == "op" && i.data["phase"] == "partial")
    );
}

#[tokio::test]
async fn settings_effort_changes_clear_ultracode_and_support_max() {
    let fx = Fixture::start("nd21-effort-rules", 3_600_000).await;
    fx.scenario
        .endpoint()
        .enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    for (id, setting) in [
        ("opus", json!({"model":"opus"})),
        ("high", json!({"effort":"high"})),
        ("ultra", json!({"ultracode":true})),
        ("medium", json!({"effort":"medium"})),
        ("max", json!({"effort":"max"})),
    ] {
        let reply = fx
            .command(
                id,
                "session.configure",
                json!({"session":session,"setting":setting}),
            )
            .await;
        assert!(
            matches!(
                reply,
                CommandReply::Receipt {
                    receipt: Receipt::Accepted { .. }
                }
            ),
            "{reply:?}"
        );
        let after = fx.wait(&session, id, |s| header(s)["op"].is_null()).await;
        eprintln!(
            "effort-rules {id}: {}",
            header(&after)["settings"]["applied"]
        );
        if id == "medium" {
            assert_eq!(header(&after)["settings"]["applied"]["effort"], "medium");
            assert_eq!(header(&after)["settings"]["applied"]["ultracode"], false);
        }
        if id == "max" {
            assert_eq!(header(&after)["settings"]["applied"]["effort"], "max");
        }
    }
    let opus = Route::new(None, "claude-opus-5-5");
    fx.scenario
        .endpoint()
        .enqueue(opus.clone(), ModelReply::text("maximum"));
    fx.send("max-turn", &session, "continue").await;
    fx.wait(&session, "max turn", |s| {
        texts(s).iter().any(|s| s == "maximum") && header(s)["process"]["turn_running"] == false
    })
    .await;
    assert_eq!(
        fx.scenario.endpoint().requests().last().unwrap().body["output_config"]["effort"],
        "max"
    );
    for (id, setting) in [
        ("on-again", json!({"ultracode":true})),
        ("haiku", json!({"model":"haiku"})),
    ] {
        let reply = fx
            .command(
                id,
                "session.configure",
                json!({"session":session,"setting":setting}),
            )
            .await;
        assert!(
            matches!(
                reply,
                CommandReply::Receipt {
                    receipt: Receipt::Accepted { .. }
                }
            ),
            "{reply:?}"
        );
        fx.wait(&session, id, |s| header(s)["op"].is_null()).await;
    }
    let after = fx.peek(&session).await;
    assert_eq!(header(&after)["caps"]["ultracode"], false);
    assert_eq!(header(&after)["settings"]["applied"]["ultracode"], false);
    if let Ok(path) = std::env::var("ND_RECORD_SETTINGS") {
        let run = header(&after)["process"]["run"].as_str().unwrap();
        let text = std::fs::read_to_string(
            fx.scenario
                .root()
                .join("recordings")
                .join(format!("{run}.jsonl")),
        )
        .unwrap();
        let records: Vec<&str> = text
            .lines()
            .filter(|line| {
                let r: Value = serde_json::from_str(line).unwrap();
                let f: Value = serde_json::from_str(r["event"]["line"].as_str().unwrap_or("{}"))
                    .unwrap_or_default();
                matches!(
                    f["type"].as_str(),
                    Some("control_request" | "control_response")
                )
            })
            .collect();
        std::fs::create_dir_all(Path::new(&path).parent().unwrap()).unwrap();
        std::fs::write(path, format!("{}\n", records.join("\n"))).unwrap();
    }
    signal(cli_pid(&fx, &after).await, rustix::process::Signal::KILL);
    fx.wait(&session, "haiku process gone", |s| {
        header(s)["process"]["alive"] == false
    })
    .await;
    let haiku = Route::new(None, "claude-haiku-4-5-20251001");
    fx.scenario
        .endpoint()
        .enqueue(haiku, ModelReply::text("restored"));
    fx.send("restore-effort", &session, "continue").await;
    fx.wait(&session, "restored", |s| {
        texts(s).iter().any(|s| s == "restored") && header(s)["process"]["turn_running"] == false
    })
    .await;
    let reply = fx
        .command(
            "opus-restored",
            "session.configure",
            json!({"session":session,"setting":{"model":"opus"}}),
        )
        .await;
    assert!(
        matches!(
            reply,
            CommandReply::Receipt {
                receipt: Receipt::Accepted { .. }
            }
        ),
        "{reply:?}"
    );
    let restored = fx
        .wait(&session, "effort restored", |s| header(s)["op"].is_null())
        .await;
    assert_eq!(header(&restored)["settings"]["applied"]["effort"], "max");
}

#[tokio::test]
async fn settings_title_waits_for_eligible_prompt_and_keeps_summary_on_empty_generation() {
    let fx = Fixture::start("nd21-title-ai-fallback", 3_600_000).await;
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(fx.main(), ModelReply::text("ready"));
    let session = fx.create("create", "/sandbox/project", "hello").await;
    fx.wait(&session, "active", |s| {
        header(s)["status"] == "active" && header(s)["process"]["turn_running"] == false
    })
    .await;
    assert_eq!(endpoint.requests().len(), 1);
    endpoint.enqueue(fx.main(), ModelReply::text("second"));
    endpoint.enqueue(fx.main(), ModelReply::text("{}"));
    fx.send("eligible", &session, "这是第一条足够长的正常人类提示")
        .await;
    let after = fx
        .wait(&session, "title fallback", |s| {
            header(s)["op"].is_null()
                && s.items
                    .iter()
                    .any(|i| i.kind == "op" && i.data["kind"] == "title")
        })
        .await;
    assert_eq!(header(&after)["title"], "hello");
    assert_eq!(header(&after)["title_source"], "summary");
    assert_eq!(endpoint.requests().len(), 3);
}

#[tokio::test]
async fn disabling_mcp_backgrounding_removes_the_send_now_preservation_promise() {
    for (name, value) in [
        ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
        ("CLAUDE_CODE_DISABLE_MCP_TASK_BACKGROUND", "true"),
        ("CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS", "0"),
    ] {
        let fx = Fixture::with_env(
            "mcp-background-disabled",
            3_600_000,
            &format!("{name} = {value:?}"),
        )
        .await;
        fx.scenario
            .endpoint()
            .enqueue(fx.main(), ModelReply::text("ready"));
        let session = fx.create("disabled", "/sandbox/project", "hello").await;
        let ready = fx.wait(&session, "ready", |s| texts(s) == ["ready"]).await;
        assert_eq!(
            header(&ready)["interaction"]["immediate_preserves_mcp"],
            false,
            "{name}"
        );
        fx.close();
    }
}
