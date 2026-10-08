mod support;
use nd_session::scripted::{ActKind, Reply};
use serde_json::json;
use support::*;

#[tokio::test]
async fn confirmed_queue_cancellation_returns_an_unknown_send_and_revokes_resend() {
    use nd_backend::{Act, Done, Outcome, Refusal};
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("unknown-cancel", "ready").await);
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    h.adapter
        .script(ActKind::Send, Reply::Unknown("echo missing".into()));
    h.send("pending", &session, "confirmed cancelled").await;
    h.wait(&session, "unknown send", |s| {
        prompt(s, "confirmed cancelled").is_some_and(|i| i.data["state"] == "unknown")
    })
    .await;
    let original = h
        .adapter
        .received()
        .into_iter()
        .find(|(_, a)| matches!(a,Act::Send { msg,.. } if msg.text == "confirmed cancelled"))
        .unwrap()
        .0;
    h.adapter
        .script(ActKind::Interrupt, Reply::Unknown("ack missing".into()));
    h.sessions
        .execute(&command(
            "cancel",
            "session.interrupt",
            json!({"session":session,"queued":"cancel"}),
        ))
        .await;
    h.wait(&session, "unknown control", |s| {
        s.items
            .iter()
            .any(|i| i.id == "control/cancel" && i.data["state"] == "unknown")
    })
    .await;
    let control = h
        .adapter
        .received()
        .into_iter()
        .find(|(_, a)| matches!(a, Act::Interrupt { .. }))
        .unwrap()
        .0;
    h.adapter.clarify(
        &control,
        Outcome::Ok {
            done: Done::Interrupted {
                already_ended: false,
                cancelled: vec![original.clone()],
            },
        },
    );
    let snapshot = h
        .wait(&session, "returned", |s| {
            prompt(s, "confirmed cancelled").is_some_and(|i| i.data["state"] == "withdrawn")
        })
        .await;
    assert_eq!(item(&snapshot, "draft").data["text"], "confirmed cancelled");
    h.adapter.clarify(
        &original,
        Outcome::Refused {
            refusal: Refusal::Lost {
                evidence: "late superseded evidence".into(),
            },
        },
    );
    // 同一适配器来源中更晚的送达事实充当观察屏障。
    h.send("later-observation", &session, "later observation")
        .await;
    h.wait(&session, "later fact", |s| {
        prompt(s, "later observation").is_some_and(|i| i.data["state"] == "landed")
    })
    .await;
    assert_eq!(
        prompt(&h.snapshot(&session), "confirmed cancelled")
            .unwrap()
            .data["state"],
        "withdrawn"
    );
    let reply = h
        .sessions
        .execute(&command(
            "resend-cancelled",
            "session.resend",
            json!({"session":session,"message":"pending"}),
        ))
        .await
        .unwrap();
    assert!(
        matches!(reply,nd_wire::CommandReply::Receipt { receipt:nd_wire::Receipt::Rejected { code,.. } } if code == "precondition")
    );
}

#[tokio::test]
async fn late_withdrawal_confirmation_restores_the_original_draft_once() {
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("clarify-withdraw", "ready").await);
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    h.adapter.script(ActKind::Send, Reply::Hold);
    h.adapter
        .script(ActKind::Withdraw, Reply::Unknown("reply lost".into()));
    h.send("pending", &session, "late return").await;
    h.sessions
        .execute(&command(
            "withdraw",
            "session.withdraw",
            json!({"session":session,"message":"pending"}),
        ))
        .await;
    h.wait(&session, "unknown", |s| {
        s.items
            .iter()
            .any(|i| i.id == "control/withdraw" && i.data["state"] == "unknown")
    })
    .await;
    let ticket = h
        .adapter
        .received()
        .into_iter()
        .find(|(_, a)| matches!(a, nd_backend::Act::Withdraw { .. }))
        .unwrap()
        .0;
    h.adapter.clarify(
        &ticket,
        nd_backend::Outcome::Ok {
            done: nd_backend::Done::Withdrawn { ok: true },
        },
    );
    let returned = h
        .wait(&session, "confirmed withdrawal", |s| {
            prompt(s, "late return").is_some_and(|i| i.data["state"] == "withdrawn")
        })
        .await;
    assert_eq!(item(&returned, "draft").data["text"], "late return");
    assert_eq!(item(&returned, "draft").data["version"], 1);
}

#[tokio::test]
async fn a_late_withdrawal_failure_cannot_erase_confirmed_delivery() {
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("late-withdraw", "ready").await);
    h.wait(&session, "created", |s| header(s)["status"] == "active")
        .await;
    h.adapter.script(ActKind::Send, Reply::Hold);
    h.adapter.script(ActKind::Withdraw, Reply::Hold);
    h.send("pending", &session, "already delivered").await;
    h.sessions
        .execute(&command(
            "withdraw",
            "session.withdraw",
            json!({"session":session,"message":"pending"}),
        ))
        .await;
    assert!(h.adapter.release(Reply::Ok));
    h.wait(&session, "confirmed delivery", |s| {
        prompt(s, "already delivered").is_some_and(|i| i.data["state"] == "landed")
    })
    .await;
    assert!(h.adapter.release(Reply::Unknown("reply lost".into())));
    let snapshot = h
        .wait(&session, "withdrawal unknown", |s| {
            s.items
                .iter()
                .any(|i| i.id == "control/withdraw" && i.data["state"] == "unknown")
        })
        .await;
    assert_eq!(
        prompt(&snapshot, "already delivered").unwrap().data["state"],
        "landed"
    );
    assert_eq!(item(&snapshot, "draft").data["text"], "");
}

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

#[tokio::test]
async fn permission_and_title_controls_remain_reachable_when_writes_are_fenced() {
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("fenced-settings", "ready").await);
    let ready = h
        .wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    h.claims
        .observe(nd_claims::Observed::IdentityMismatch {
            run: header(&ready)["process"]["run"].as_str().unwrap().into(),
        })
        .unwrap();
    h.sessions
        .execute(&command(
            "mode",
            "session.configure",
            json!({"session":session,"setting":{"permission_mode":"acceptEdits"}}),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        h.wait(&session, "permission changed", |s| {
            header(s)["permission_mode"] == "acceptEdits" && header(s)["op"].is_null()
        }),
    )
    .await
    .unwrap();
    h.sessions
        .execute(&command(
            "rename",
            "session.rename",
            json!({"session":session,"title":"control still works"}),
        ))
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        h.wait(&session, "title changed", |s| {
            header(s)["title"] == "control still works" && header(s)["op"].is_null()
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn withdrawing_a_send_refused_as_busy_returns_it_locally_without_a_backend_cancel() {
    let mut cfg = config();
    cfg.tick = std::time::Duration::from_secs(30);
    let h = Harness::new(cfg).await;
    let session = accepted_session(&h.create("busy-withdraw", "ready").await);
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    for _ in 0..100 {
        h.adapter.script(ActKind::Send, Reply::Busy);
    }
    h.send("busy-message", &session, "never handed to backend")
        .await;
    h.sessions
        .execute(&command(
            "withdraw-busy",
            "session.withdraw",
            json!({"session":session,"message":"busy-message"}),
        ))
        .await;
    let s = h
        .wait(&session, "withdrawn", |s| {
            prompt(s, "never handed to backend").is_some_and(|p| p.data["state"] == "withdrawn")
        })
        .await;
    assert_eq!(item(&s, "draft").data["text"], "never handed to backend");
    assert!(
        !h.adapter
            .received()
            .iter()
            .any(|(_, a)| matches!(a, nd_backend::Act::Withdraw { .. }))
    );
    assert!(
        !h.adapter
            .applied()
            .iter()
            .any(|a| a == "send:never handed to backend")
    );
}
