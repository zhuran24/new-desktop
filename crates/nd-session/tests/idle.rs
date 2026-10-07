//! 窄接缝一：忙碌输入持续压过 Tick 时仍必须重新计闲置期限。
mod support;
use nd_backend::Act;
use nd_session::scripted::{ActKind, Reply};
use serde_json::json;
use std::time::Duration;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn busy_inputs_reset_idle_time_without_waiting_for_a_tick() {
    let mut cfg = config();
    cfg.tick = Duration::from_millis(500);
    cfg.idle_reclaim = Duration::from_millis(1500);
    let h = Harness::new(cfg).await;
    let session = accepted_session(&h.create("idle-reset", "first").await);
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    tokio::time::sleep(Duration::from_millis(650)).await;
    h.adapter.script(ActKind::Send, Reply::Hold);
    h.send("busy", &session, "busy").await;
    h.wait(&session, "pending", |s| {
        prompt(s, "busy").is_some_and(|p| p.data["state"] == "pending")
    })
    .await;
    let start = tokio::time::Instant::now();
    let mut version = 0;
    // 每个事务都可见忙碌状态；连续输入让执行器的空闲等待达不到 Tick。
    while start.elapsed() < Duration::from_millis(2100) {
        let mut c = command(
            &format!("edit-{version}"),
            "session.draft.update",
            json!({"session":session,"text":format!("draft-{version}"),"attachments":[]}),
        );
        c.expect = json!({"draft_version":version});
        h.sessions.execute(&c).await.unwrap();
        version += 1;
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    assert!(h.adapter.release(Reply::Ok));
    h.wait(&session, "landed", |s| {
        prompt(s, "busy").is_some_and(|p| p.data["state"] == "landed")
    })
    .await;
    let finished = tokio::time::Instant::now();
    loop {
        if h.adapter
            .received()
            .iter()
            .any(|(_, act)| matches!(act, Act::End { .. }))
        {
            assert!(
                finished.elapsed() >= Duration::from_millis(1400),
                "busy interval was counted as idle: {:?}",
                finished.elapsed()
            );
            break;
        }
        assert!(
            finished.elapsed() < Duration::from_secs(5),
            "idle process was never reclaimed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
