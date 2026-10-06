mod support;
use nd_session::scripted::{ActKind, Reply};
use serde_json::json;
use support::*;

#[tokio::test]
async fn unknown_withdrawal_does_not_refill_the_draft_or_retry_the_send() {
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("unknown-withdraw", "ready").await);
    h.wait(&session, "created", |s| header(s)["status"] == "active")
        .await;
    h.adapter.script(ActKind::Send, Reply::Hold);
    h.adapter
        .script(ActKind::Withdraw, Reply::Unknown("reply lost".into()));
    h.send("pending", &session, "possibly withdrawn").await;
    h.sessions
        .execute(&command(
            "withdraw",
            "session.withdraw",
            json!({"session":session,"message":"pending"}),
        ))
        .await;
    let snapshot = h
        .wait(&session, "unknown", |s| {
            s.items
                .iter()
                .any(|i| i.id == "control/withdraw" && i.data["state"] == "unknown")
        })
        .await;
    assert_eq!(item(&snapshot, "draft").data["text"], "");
    assert_eq!(
        prompt(&snapshot, "possibly withdrawn").unwrap().data["state"],
        "unknown"
    );
    assert!(h.adapter.release(Reply::Ok));
    h.wait(&session, "late original delivery", |s| {
        prompt(s, "possibly withdrawn").is_some_and(|i| i.data["state"] == "landed")
    })
    .await;
}

#[tokio::test]
async fn a_busy_send_waits_while_interrupt_is_still_accepted() {
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("busy-control", "ready").await);
    h.wait(&session, "created", |s| header(s)["status"] == "active")
        .await;
    h.adapter.script(ActKind::Send, Reply::Busy);
    h.adapter.script(ActKind::Send, Reply::Hold);
    h.send("busy-send", &session, "wait for room").await;
    h.sessions
        .execute(&command(
            "esc",
            "session.interrupt",
            json!({"session":session}),
        ))
        .await;
    h.wait(&session, "control accepted", |s| {
        s.items
            .iter()
            .any(|i| i.id == "control/esc" && i.data["state"] == "acknowledged")
    })
    .await;
    h.wait(&session, "send still pending", |s| {
        prompt(s, "wait for room").is_some_and(|i| i.data["state"] == "pending")
    })
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !h.adapter.release(Reply::Ok) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    h.wait(&session, "send landed", |s| {
        prompt(s, "wait for room").is_some_and(|i| i.data["state"] == "landed")
    })
    .await;
}
