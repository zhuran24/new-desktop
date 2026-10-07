#![cfg(feature = "scenarios")]
use nd_testkit::{Scenario, ScenarioOptions};
use nd_ui_core::SyncReplica;
use std::time::Duration;

#[tokio::test]
async fn cold_start_gets_a_snapshot_even_without_events() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let snapshot = tokio::time::timeout(Duration::from_secs(2), replica.subscribe("global"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.stream, "global");
    assert_eq!(snapshot.cursor, 0);
    assert_eq!(snapshot.items[0].fallback.title, "New Desktop");
    assert!(!snapshot.epoch.is_empty());
}

#[tokio::test]
async fn live_replica_replaces_snapshot_after_kill_and_service_restart() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let mut previous = replica.subscribe("global").await.unwrap();
    for kill in [true, false] {
        if kill {
            daemon.kill_daemon().unwrap();
        } else {
            daemon.restart_daemon().unwrap();
        }
        let current = tokio::time::timeout(Duration::from_secs(5), replica.next())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(current.epoch, previous.epoch);
        assert_eq!(current.cursor, 0);
        assert_eq!(
            current.items[0].data["config"]["value"],
            previous.items[0].data["config"]["value"]
        );
        assert_eq!(current.items[1..], previous.items[1..]);
        previous = current;
    }
}

#[tokio::test]
async fn local_socket_lives_in_owner_only_directory() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let meta = std::fs::metadata(daemon.root().join("runtime")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    assert_eq!(meta.uid(), rustix::process::geteuid().as_raw());
    SyncReplica::connect(&daemon.socket()).await.unwrap();
}

#[tokio::test]
async fn config_unloads_optional_namespace_and_command_without_losing_global() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let initial = replica.subscribe("global").await.unwrap();
    assert!(initial.items.iter().any(|i| i.namespace == "diagnostics"));
    assert!(replica.query("diagnostics.inspect").await.is_ok());
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled = false\n",
    )
    .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = replica.next().await.unwrap();
            if !s.items.iter().any(|i| i.namespace == "diagnostics") {
                break s;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        changed.items[0].data["components"]["diagnostics"]["state"],
        "disabled"
    );
    assert!(changed.cursor > initial.cursor);
    assert_eq!(changed.epoch, initial.epoch);
    assert!(changed.items.iter().any(|i| i.namespace == "system"));
    assert!(replica.query("diagnostics.inspect").await.is_err());
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled = true\n",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if replica
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
    assert!(replica.query("diagnostics.inspect").await.is_ok());
}

#[tokio::test]
async fn ndctl_get_and_watch_share_the_replica_and_get_pages() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ndctl"))
        .env_clear()
        .args([
            "--socket",
            daemon.socket().to_str().unwrap(),
            "get",
            "global",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let snapshot: nd_wire::Snapshot = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshot.stream, "global");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ndctl"))
        .env_clear()
        .args([
            "--socket",
            daemon.socket().to_str().unwrap(),
            "watch",
            "global",
        ])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<nd_wire::Snapshot>(&first).unwrap(),
        snapshot
    );
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled = false\n",
    )
    .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !serde_json::from_str::<nd_wire::Snapshot>(&second)
            .unwrap()
            .items
            .iter()
            .any(|i| i.namespace == "diagnostics")
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let page = replica
        .get("system", nd_wire::PageReq::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].fallback.title, "New Desktop");
    assert!(page.next.is_none());
    assert!(
        replica
            .get("diagnostics", nd_wire::PageReq::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn attachment_roundtrip_uses_authenticated_http_and_survives_restart() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let id = replica.put_blob(b"abc").await.unwrap();
    assert_eq!(
        id,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(replica.get_blob(&id).await.unwrap(), b"abc");
    daemon.restart_daemon().unwrap();
    drop(daemon.connect().await.unwrap());
    assert_eq!(replica.get_blob(&id).await.unwrap(), b"abc");
    assert!(replica.get_blob("../config.toml").await.is_err());
}

#[tokio::test]
async fn short_disconnect_replays_events_and_wrong_epoch_forces_snapshot() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let initial = replica.subscribe("global").await.unwrap();
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled = false\n",
    )
    .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(3), replica.next())
        .await
        .unwrap()
        .unwrap();
    let (mut wire, _) = tokio_tungstenite::client_async(
        "ws://localhost/wire",
        tokio::net::UnixStream::connect(&daemon.socket())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    wire.send(Message::Text(
        serde_json::to_string(&nd_wire::Request::Hello {
            version: 1,
            namespaces: Default::default(),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    wire.next().await.unwrap().unwrap();
    wire.send(Message::Text(
        serde_json::to_string(&nd_wire::Request::Subscribe {
            stream: "global".into(),
            since: Some(initial.position()),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let first: nd_wire::Response =
        serde_json::from_str(wire.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let nd_wire::Response::Event { event } = first else {
        panic!("same epoch must replay: {first:?}")
    };
    assert_eq!(event.cursor, changed.cursor);
    assert!(event.remove.contains(&"diagnostics".to_owned()));
    wire.close(None).await.unwrap();
    // 对未来游标和旧纪元同样不能声称补齐。
    for cursor in [
        nd_wire::Cursor {
            epoch: initial.epoch,
            seq: u64::MAX,
        },
        nd_wire::Cursor {
            epoch: "old-epoch".into(),
            seq: 0,
        },
    ] {
        let (mut wire, _) = tokio_tungstenite::client_async(
            "ws://localhost/wire",
            tokio::net::UnixStream::connect(&daemon.socket())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        wire.send(Message::Text(
            serde_json::to_string(&nd_wire::Request::Hello {
                version: 1,
                namespaces: Default::default(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        wire.next().await.unwrap().unwrap();
        wire.send(Message::Text(
            serde_json::to_string(&nd_wire::Request::Subscribe {
                stream: "global".into(),
                since: Some(cursor),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        let response: nd_wire::Response =
            serde_json::from_str(wire.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(matches!(response, nd_wire::Response::Snapshot { .. }));
    }
}

#[tokio::test]
async fn invalid_external_config_keeps_last_good_component_and_reports_error() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    replica.subscribe("global").await.unwrap();
    std::fs::write(
        daemon.root().join("config.toml"),
        "[diagnostics]\nenabled = 'invalid'\n",
    )
    .unwrap();
    let invalid = tokio::time::timeout(Duration::from_secs(3), replica.next())
        .await
        .unwrap()
        .unwrap();
    assert!(invalid.items.iter().any(|i| i.namespace == "diagnostics"));
    assert!(invalid.items[0].data["config_error"].is_string());
    // 原子替换也是受支持的文件编辑方式。
    std::fs::write(
        daemon.root().join("next.toml"),
        "[diagnostics]\nenabled = false\n",
    )
    .unwrap();
    std::fs::rename(
        daemon.root().join("next.toml"),
        daemon.root().join("config.toml"),
    )
    .unwrap();
    let repaired = tokio::time::timeout(Duration::from_secs(3), replica.next())
        .await
        .unwrap()
        .unwrap();
    assert!(!repaired.items.iter().any(|i| i.namespace == "diagnostics"));
    assert!(repaired.items[0].data["config_error"].is_null());
}

#[tokio::test]
async fn cancelled_query_reply_is_not_mistaken_for_the_next_query() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    {
        let mut query = Box::pin(replica.get("diagnostics", nd_wire::PageReq::default()));
        assert!(futures::poll!(&mut query).is_pending());
    }
    let page = replica
        .get("system", nd_wire::PageReq::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].namespace, "system");
}

#[tokio::test]
async fn cursor_older_than_retained_events_gets_complete_snapshot() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    let initial = replica.subscribe("global").await.unwrap();
    for n in 1..=130 {
        std::fs::write(daemon.root().join("next.toml"), format!("counter = {n}\n")).unwrap();
        std::fs::rename(
            daemon.root().join("next.toml"),
            daemon.root().join("config.toml"),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let s = replica.next().await.unwrap();
                if s.items[0].data["config"]["value"]["counter"] == n {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    let (mut wire, _) = tokio_tungstenite::client_async(
        "ws://localhost/wire",
        tokio::net::UnixStream::connect(&daemon.socket())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    wire.send(Message::Text(
        serde_json::to_string(&nd_wire::Request::Hello {
            version: 1,
            namespaces: Default::default(),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    wire.next().await.unwrap().unwrap();
    wire.send(Message::Text(
        serde_json::to_string(&nd_wire::Request::Subscribe {
            stream: "global".into(),
            since: Some(initial.position()),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let response: nd_wire::Response =
        serde_json::from_str(wire.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let nd_wire::Response::Snapshot { snapshot } = response else {
        panic!("expired cursor needs snapshot")
    };
    assert_eq!(snapshot.items[0].data["config"]["value"]["counter"], 130);
    assert!(snapshot.items.iter().any(|i| i.namespace == "diagnostics"));
}

#[tokio::test]
async fn peer_uid_rejects_another_uid_even_if_socket_permissions_are_relaxed() {
    use std::os::unix::fs::PermissionsExt;
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    // 只放宽这一个测试实例，让异 uid 到达 accept，验证身份检查而不是文件权限。
    for path in [
        daemon.root().to_owned(),
        daemon.root().join("runtime"),
        daemon.socket().clone(),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    // 场景根位于 owner 私有 runtime 父目录；只在新的 mount namespace 内把本测试 socket
    // 绑定到公开临时路径，再降到异 uid。绝不放宽 owner 的 /run/user/<uid>。
    let accessible = tempfile::tempdir().unwrap();
    std::fs::set_permissions(accessible.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let result = tokio::process::Command::new("/usr/bin/unshare")
        .env_clear().env("PATH", "/usr/bin")
        .args(["--user", "--map-auto", "--map-root-user", "--mount", "--net", "--", "/usr/bin/python3", "-c",
            "import socket,sys,os,subprocess\nsubprocess.run(['mount','--bind',sys.argv[1],sys.argv[2]],check=True)\nos.setgid(1);os.setuid(1)\ns=socket.socket(socket.AF_UNIX);s.settimeout(2);s.connect(sys.argv[2]+'/nd.sock')\ntry:\n s.sendall(b'GET /wire HTTP/1.1\\r\\nHost: localhost\\r\\n\\r\\n'); data=s.recv(4096); assert data==b'',data\nexcept ConnectionResetError: pass\nprint('rejected after connect')"])
        .arg(daemon.socket().parent().unwrap()).arg(accessible.path())
        .output().await.unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "rejected after connect"
    );
    SyncReplica::connect(&daemon.socket()).await.unwrap();
}

#[tokio::test]
async fn default_xdg_layout_and_scenario_cgroup_are_isolated_and_cleaned() {
    let daemon = Scenario::start(
        ScenarioOptions::new("wire", env!("CARGO_BIN_EXE_nd-daemon")).default_paths(),
    )
    .await
    .unwrap();
    assert_eq!(daemon.limits().unwrap().memory_max, 2 * 1024 * 1024 * 1024);
    assert_eq!(daemon.limits().unwrap().memory_swap_max, 0);
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    assert_eq!(replica.subscribe("global").await.unwrap().stream, "global");
    assert!(
        daemon
            .root()
            .join("state/new-desktop/state.sqlite")
            .exists()
    );
    assert!(!daemon.root().join("state.sqlite").exists());
    let units = daemon.units();
    let root = daemon.root().to_owned();
    drop(replica);
    drop(daemon);
    assert!(!root.exists());
    for unit in units {
        let output = std::process::Command::new("systemctl")
            .args(["--user", "is-active", &unit])
            .output()
            .unwrap();
        assert!(!output.status.success(), "unit still active: {unit}");
    }
}

#[tokio::test]
async fn hello_negotiates_only_supported_namespace_versions() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let (mut wire, _) = tokio_tungstenite::client_async(
        "ws://localhost/wire",
        tokio::net::UnixStream::connect(&daemon.socket())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    wire.send(Message::Text(
        serde_json::to_string(&nd_wire::Request::Hello {
            version: 1,
            namespaces: std::collections::BTreeMap::from([
                ("system".into(), 4),
                ("diagnostics".into(), 0),
                ("future".into(), 1),
            ]),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let response: nd_wire::Response =
        serde_json::from_str(wire.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let nd_wire::Response::Hello { namespaces, .. } = response else {
        panic!("hello")
    };
    assert_eq!(
        namespaces,
        std::collections::BTreeMap::from([("system".into(), 1)])
    );
}

#[tokio::test]
async fn unreferenced_upload_is_collected_after_configured_grace() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut replica = SyncReplica::connect(&daemon.socket()).await.unwrap();
    replica.subscribe("global").await.unwrap();
    let id = replica.put_blob(b"unreferenced upload").await.unwrap();
    std::fs::write(
        daemon.root().join("config.toml"),
        "[storage]\nblob_grace_seconds = 0\ngc_interval_seconds = 1\n",
    )
    .unwrap();
    replica.next().await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if replica.get_blob(&id).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    // 连接仍有效，排除服务退出造成的假阳性。
    replica
        .get("system", nd_wire::PageReq::default())
        .await
        .unwrap();
}
