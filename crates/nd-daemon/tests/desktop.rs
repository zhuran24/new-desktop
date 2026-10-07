#![cfg(feature = "scenarios")]
use nd_testkit::{Scenario, ScenarioOptions};
use nd_ui_core::{FeedUpdate, ReplicaFeed, SyncReplica};
use std::time::Duration;

async fn snapshot(feed: &mut ReplicaFeed) -> nd_wire::Snapshot {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let FeedUpdate::Snapshot(snapshot) = feed.recv().await.unwrap() {
                return snapshot;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn desktop_cold_reopen_and_daemon_restart_match_authoritative_snapshot_with_zero_events() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut feed = ReplicaFeed::start(daemon.socket(), "global").unwrap();
    let first = snapshot(&mut feed).await;
    assert_eq!(first.cursor, 0);
    assert!(!first.items.is_empty());
    // 故障从外面造：丢掉整个界面同步副本，保留的视图状态不能补事实。
    drop(feed);
    let mut feed = ReplicaFeed::start(daemon.socket(), "global").unwrap();
    assert_eq!(snapshot(&mut feed).await, first);
    for kill in [true, false] {
        if kill {
            daemon.kill_daemon().unwrap();
        } else {
            daemon.restart_daemon().unwrap();
        }
        let changed = snapshot(&mut feed).await;
        assert_ne!(changed.epoch, first.epoch);
        assert_eq!(changed.cursor, 0);
        let mut reference = SyncReplica::connect(&daemon.socket()).await.unwrap();
        assert_eq!(changed, reference.subscribe("global").await.unwrap());
        let mut reopened = ReplicaFeed::start(daemon.socket(), "global").unwrap();
        assert_eq!(snapshot(&mut reopened).await, changed);
        reopened.close().await;
    }
    feed.close().await;
    let mut still_running = SyncReplica::connect(&daemon.socket()).await.unwrap();
    assert_eq!(still_running.subscribe("global").await.unwrap().cursor, 0);
}

#[tokio::test]
async fn closing_a_backpressured_desktop_feed_does_not_wait_for_the_desktop_or_stop_the_daemon() {
    let daemon = Scenario::start(ScenarioOptions::new(
        "wire",
        env!("CARGO_BIN_EXE_nd-daemon"),
    ))
    .await
    .unwrap();
    let mut feed = ReplicaFeed::start(daemon.socket(), "global").unwrap();
    snapshot(&mut feed).await;
    let mut writer = SyncReplica::connect(&daemon.socket()).await.unwrap();
    for revision in 0..8 {
        let reply = writer
            .command(&nd_wire::Command {
                id: format!("desktop-backpressure-{revision}"),
                device: "desktop-test".into(),
                name: "diagnostics.set_note".into(),
                args: serde_json::json!({"text": format!("note-{revision}")}),
                expect: serde_json::json!({"revision": revision}),
            })
            .await
            .unwrap();
        assert!(matches!(reply, nd_wire::CommandReply::Receipt { .. }));
    }
    tokio::time::timeout(Duration::from_secs(1), feed.close())
        .await
        .unwrap();
    let mut reopened = ReplicaFeed::start(daemon.socket(), "global").unwrap();
    let current = snapshot(&mut reopened).await;
    let mut reference = SyncReplica::connect(&daemon.socket()).await.unwrap();
    assert_eq!(current, reference.subscribe("global").await.unwrap());
    reopened.close().await;
}
