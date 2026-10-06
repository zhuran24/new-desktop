#![cfg(feature = "scenarios")]
mod support;
use nd_ui_core::SyncReplica;
use nd_wire::{Command, CommandReply, Receipt, ReceiptLookup};
use serde_json::json;

fn note(id: &str, text: &str, revision: u64) -> Command {
    Command {
        id: id.into(),
        device: "desktop-test".into(),
        name: "diagnostics.set_note".into(),
        args: json!({"text": text}),
        expect: json!({"revision": revision}),
    }
}

#[tokio::test]
async fn duplicate_returns_original_receipt_before_rechecking_preconditions() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    ui.subscribe("global").await.unwrap();
    let command = note("note-1", "检查连接", 0);
    let first = ui.command(&command).await.unwrap();
    assert_eq!(
        first,
        CommandReply::Receipt {
            receipt: Receipt::Done {
                value: json!({"revision": 1})
            }
        }
    );
    assert_eq!(ui.command(&command).await.unwrap(), first);
    let page = ui.get("diagnostics", Default::default()).await.unwrap();
    assert_eq!(
        page.items[0].data["note"],
        json!({"text":"检查连接", "revision":1})
    );
    assert_eq!(
        ui.receipt("note-1").await.unwrap(),
        ReceiptLookup::Found {
            receipt: Receipt::Done {
                value: json!({"revision": 1})
            }
        }
    );
}

#[tokio::test]
async fn reused_id_with_changed_content_conflicts_even_from_another_device() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    let command = note("shared-id", "first", 0);
    ui.command(&command).await.unwrap();
    for field in ["args", "expect", "device", "name"] {
        let mut changed = command.clone();
        match field {
            "args" => changed.args = json!({"text":"different"}),
            "expect" => changed.expect = json!({"revision":1}),
            "device" => changed.device = "phone".into(),
            _ => changed.name = "future.command".into(),
        }
        assert_eq!(
            ui.command(&changed).await.unwrap(),
            CommandReply::Conflict,
            "{field}"
        );
    }
    assert_eq!(
        ui.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"],
        json!({"text":"first","revision":1})
    );
}

#[tokio::test]
async fn invalid_or_stale_preconditions_are_durable_rejections_without_effects() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    let mut missing = note("missing", "no", 0);
    missing.expect = json!({});
    assert_eq!(
        ui.command(&missing).await.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Rejected {
                code: "invalid".into(),
                now: serde_json::Value::Null
            }
        }
    );
    let stale = note("stale", "no", 1);
    let refused = ui.command(&stale).await.unwrap();
    assert_eq!(
        refused,
        CommandReply::Receipt {
            receipt: Receipt::Rejected {
                code: "precondition".into(),
                now: json!({"revision":0})
            }
        }
    );
    ui.command(&note("valid", "yes", 0)).await.unwrap();
    assert_eq!(ui.command(&stale).await.unwrap(), refused);
    assert_eq!(
        ui.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"],
        json!({"text":"yes","revision":1})
    );
}

#[tokio::test]
async fn expired_receipt_leaves_a_tombstone_across_restart() {
    let daemon = support::Daemon::configured("[commands]\nreceipt_keep_ms = 50\n").await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    let command = note("old", "retained effect", 0);
    ui.command(&command).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(ui.receipt("old").await.unwrap(), ReceiptLookup::Expired);
    assert_eq!(ui.command(&command).await.unwrap(), CommandReply::Expired);
    daemon.restart();
    daemon.wait_ready().await;
    let mut fresh = SyncReplica::connect(&daemon.socket).await.unwrap();
    assert_eq!(
        fresh.command(&command).await.unwrap(),
        CommandReply::Expired
    );
    assert_eq!(
        fresh.command(&note("old", "different", 0)).await.unwrap(),
        CommandReply::Conflict
    );
    assert_eq!(
        fresh
            .get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"],
        json!({"text":"retained effect","revision":1})
    );
}

#[tokio::test]
async fn crash_at_commit_boundaries_never_separates_effect_from_receipt() {
    for point in ["after_effect", "before_commit", "after_commit"] {
        let daemon = support::Daemon::start().await;
        let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
        ui.subscribe("global").await.unwrap();
        std::fs::write(
            daemon.root().join("command-fault.json"),
            json!({"id":"crash", "point":point, "action":"crash"}).to_string(),
        )
        .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            ui.command(&note("crash", "atomic", 0)),
        )
        .await
        .unwrap()
        .unwrap();
        if point == "after_commit" {
            assert_eq!(
                result,
                CommandReply::Receipt {
                    receipt: Receipt::Done {
                        value: json!({"revision":1})
                    }
                }
            );
        } else {
            assert_eq!(result, CommandReply::DeliveryUnknown);
            assert_eq!(ui.receipt("crash").await.unwrap(), ReceiptLookup::Missing);
        }
        let expected = if point == "after_commit" {
            json!({"text":"atomic","revision":1})
        } else {
            json!({"text":"","revision":0})
        };
        assert_eq!(
            ui.get("diagnostics", Default::default())
                .await
                .unwrap()
                .items[0]
                .data["note"],
            expected
        );
        assert!(
            !daemon.root().join("command-fault.json").exists(),
            "故障点必须实际到达"
        );
    }
}

#[tokio::test]
async fn unavailable_rolls_back_then_retries_the_same_id_with_backoff() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    std::fs::write(
        daemon.root().join("command-fault.json"),
        json!({"id":"retry", "point":"after_effect", "action":"unavailable"}).to_string(),
    )
    .unwrap();
    let started = tokio::time::Instant::now();
    assert_eq!(
        ui.command(&note("retry", "once", 0)).await.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Done {
                value: json!({"revision":1})
            }
        }
    );
    assert!(started.elapsed() >= std::time::Duration::from_millis(50));
    assert_eq!(
        ui.receipt("retry").await.unwrap(),
        ReceiptLookup::Found {
            receipt: Receipt::Done {
                value: json!({"revision":1})
            }
        }
    );
    assert_eq!(
        ui.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"],
        json!({"text":"once","revision":1})
    );
}

#[tokio::test]
async fn slow_connection_is_bounded_while_other_replicas_keep_event_order() {
    let daemon =
        support::Daemon::configured("[wire]\nsend_queue = 4\nsend_timeout_ms = 50\n").await;
    let mut slow = SyncReplica::connect(&daemon.socket).await.unwrap();
    slow.subscribe("global").await.unwrap();
    let mut healthy = SyncReplica::connect(&daemon.socket).await.unwrap();
    healthy.subscribe("global").await.unwrap();
    let mut writer = SyncReplica::connect(&daemon.socket).await.unwrap();
    let produce = async {
        for revision in 0..40 {
            let text = format!("{revision}:{}", "x".repeat(60000));
            assert!(matches!(
                writer
                    .command(&note(&format!("flood-{revision}"), &text, revision))
                    .await
                    .unwrap(),
                CommandReply::Receipt {
                    receipt: Receipt::Done { .. }
                }
            ));
        }
    };
    let consume = async {
        for expected in 1..=40 {
            let snapshot = healthy.next().await.unwrap();
            assert_eq!(snapshot.cursor, expected);
            assert_eq!(
                snapshot
                    .items
                    .iter()
                    .find(|i| i.id == "diagnostics")
                    .unwrap()
                    .data["note"]["revision"],
                expected
            );
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(produce, consume);
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        slow.query("diagnostics.inspect").await.is_err(),
        "慢连接应被断开"
    );
    assert!(matches!(
        slow.receipt("flood-39").await.unwrap(),
        ReceiptLookup::Found { .. }
    ));
    assert_eq!(
        slow.current("global")
            .unwrap()
            .items
            .iter()
            .find(|i| i.id == "diagnostics")
            .unwrap()
            .data["note"]["revision"],
        40
    );
}

#[tokio::test]
async fn ndctl_submits_and_queries_the_same_durable_receipt() {
    let daemon = support::Daemon::start().await;
    let command = serde_json::to_string(&note("ndctl", "from CLI", 0)).unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ndctl"))
        .env_clear()
        .args([
            "--socket",
            daemon.socket.to_str().unwrap(),
            "command",
            &command,
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<CommandReply>(&output.stdout).unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Done {
                value: json!({"revision":1})
            }
        }
    );
    daemon.kill();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    daemon.wait_ready().await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ndctl"))
        .env_clear()
        .args([
            "--socket",
            daemon.socket.to_str().unwrap(),
            "receipt",
            "ndctl",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<ReceiptLookup>(&output.stdout).unwrap(),
        ReceiptLookup::Found {
            receipt: Receipt::Done {
                value: json!({"revision":1})
            }
        }
    );
}

#[tokio::test]
async fn lost_conflict_response_is_not_mistaken_for_another_commands_receipt() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    ui.command(&note("collision", "first", 0)).await.unwrap();
    std::fs::write(
        daemon.root().join("command-fault.json"),
        json!({"id":"collision", "point":"after_commit", "action":"crash"}).to_string(),
    )
    .unwrap();
    assert_eq!(
        ui.command(&note("collision", "different", 0))
            .await
            .unwrap(),
        CommandReply::Conflict
    );
}

#[tokio::test]
async fn simultaneous_devices_serialize_preconditions_and_duplicate_submissions() {
    let daemon = support::Daemon::start().await;
    let mut left = SyncReplica::connect(&daemon.socket).await.unwrap();
    let mut right = SyncReplica::connect(&daemon.socket).await.unwrap();
    let one = note("left", "winner left", 0);
    let mut two = note("right", "winner right", 0);
    two.device = "phone-test".into();
    let (a, b) = tokio::join!(left.command(&one), right.command(&two));
    let replies = [a.unwrap(), b.unwrap()];
    assert_eq!(
        replies
            .iter()
            .filter(|r| matches!(
                r,
                CommandReply::Receipt {
                    receipt: Receipt::Done { .. }
                }
            ))
            .count(),
        1
    );
    assert_eq!(replies.iter().filter(|r| matches!(r, CommandReply::Receipt { receipt:Receipt::Rejected { code, .. } } if code=="precondition")).count(), 1);
    let next = note("shared", "once", 1);
    let (a, b) = tokio::join!(left.command(&next), right.command(&next));
    let expected = CommandReply::Receipt {
        receipt: Receipt::Done {
            value: json!({"revision":2}),
        },
    };
    assert_eq!(a.unwrap(), expected);
    assert_eq!(b.unwrap(), expected);
    assert_eq!(
        left.get("diagnostics", Default::default())
            .await
            .unwrap()
            .items[0]
            .data["note"],
        json!({"text":"once","revision":2})
    );
}

#[tokio::test]
async fn unloading_a_provider_retracts_new_commands_but_keeps_receipts() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    ui.subscribe("global").await.unwrap();
    let command = note("before-unload", "saved", 0);
    let receipt = ui.command(&command).await.unwrap();
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled=false\n",
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if !ui
                .next()
                .await
                .unwrap()
                .items
                .iter()
                .any(|i| i.namespace == "diagnostics")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ui.command(&command).await.unwrap(), receipt);
    assert!(matches!(
        ui.receipt(&command.id).await.unwrap(),
        ReceiptLookup::Found { .. }
    ));
    assert_eq!(
        ui.command(&note("after-unload", "no", 1)).await.unwrap(),
        CommandReply::Receipt {
            receipt: Receipt::Rejected {
                code: "not_found".into(),
                now: serde_json::Value::Null
            }
        }
    );
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled=true\n",
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let snapshot = ui.next().await.unwrap();
            if let Some(item) = snapshot.items.iter().find(|i| i.namespace == "diagnostics") {
                assert_eq!(item.data["note"], json!({"text":"saved","revision":1}));
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn json_key_order_does_not_change_command_identity() {
    let daemon = support::Daemon::start().await;
    let mut ui = SyncReplica::connect(&daemon.socket).await.unwrap();
    // 未知命令也产生不可变拒绝收据；嵌套正文以不同对象键顺序重试。
    let a: Command = serde_json::from_str(r#"{"id":"ordered","device":"d","name":"future.op","args":{"a":1,"b":{"c":2,"d":3}},"expect":{}}"#).unwrap();
    let b: Command = serde_json::from_str(r#"{"expect":{},"args":{"b":{"d":3,"c":2},"a":1},"name":"future.op","device":"d","id":"ordered"}"#).unwrap();
    let result = ui.command(&a).await.unwrap();
    assert_eq!(ui.command(&b).await.unwrap(), result);
    assert!(matches!(
        result,
        CommandReply::Receipt {
            receipt: Receipt::Rejected { .. }
        }
    ));
}
