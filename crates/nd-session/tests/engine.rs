//! 窄接缝一：经名册的命令和会话流驱动真的会话组件、独占登记与 SQLite，后端是脚本化适配器。
mod support;
use nd_backend::Act;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_created_session_becomes_active_once_its_first_message_lands() {
    let h = Harness::new(config()).await;
    let reply = h.create("create-1", "你好").await;
    let session = accepted_session(&reply);
    let snapshot = h
        .wait(&session, "session active", |s| {
            header(s)["status"] == "active"
        })
        .await;
    let first = prompt(&snapshot, "你好").expect("first prompt shown");
    assert_eq!(first.data["state"], "landed");
    let received = h.adapter.received();
    assert!(matches!(received[0].1, Act::Open { .. }), "{received:?}");
    assert!(
        matches!(&received[1].1, Act::Send { msg, .. } if msg.text == "你好"),
        "{received:?}"
    );
    // 回显的原生编号就是这张票派生的那个。
    assert_eq!(
        first.data["native"],
        nd_backend::native_uuid(&received[1].0)
    );
    let listed = h.sessions.listing().items();
    let entry = listed
        .iter()
        .find(|i| i.id == format!("session/{session}"))
        .expect("listed");
    assert_eq!(entry.data["status"], "active");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_never_started_a_backend_is_withdrawn_and_noticed_once() {
    let h = Harness::new(config()).await;
    h.adapter.script(
        nd_session::scripted::ActKind::Open,
        nd_session::scripted::Reply::Fail("工作目录不存在".into()),
    );
    let session = accepted_session(&h.create("create-bad", "你好").await);
    let snapshot = h
        .wait(&session, "withdrawn", |s| {
            s.items
                .iter()
                .any(|i| i.id == "header" && i.data["status"] == "withdrawn")
        })
        .await;
    assert!(
        header(&snapshot)["note"]
            .as_str()
            .unwrap()
            .contains("工作目录不存在")
    );
    // 没做过不可逆步骤：没有任何原生步骤生效，首条消息从没写出。
    assert!(h.adapter.applied().is_empty(), "{:?}", h.adapter.applied());
    let listed = h.sessions.listing().items();
    assert!(!listed.iter().any(|i| i.id == format!("session/{session}")));
    let notices: Vec<_> = listed
        .iter()
        .filter(|i| i.kind == "notice" && i.data["session"] == session.0)
        .collect();
    assert_eq!(notices.len(), 1, "{listed:#?}");
    // 撤掉的会话不再收消息。
    let reply = h.send("send-to-withdrawn", &session, "还在吗").await;
    assert!(
        matches!(&reply, nd_wire::CommandReply::Receipt { receipt: nd_wire::Receipt::Rejected { code, .. } } if code == "precondition"),
        "{reply:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_whose_first_message_may_have_reached_the_backend_is_kept_as_partial() {
    let h = Harness::new(config()).await;
    h.adapter.script(
        nd_session::scripted::ActKind::Send,
        nd_session::scripted::Reply::Hold,
    );
    let session = accepted_session(&h.create("create-partial", "你好").await);
    h.wait(&session, "first message pending", |s| {
        prompt(s, "你好").is_some()
    })
    .await;
    h.send("held-before-partial", &session, "during creation")
        .await;
    h.wait(&session, "held", |s| {
        prompt(s, "during creation").is_some_and(|p| p.data["state"] == "held")
    })
    .await;
    assert!(h.adapter.release(nd_session::scripted::Reply::Unknown(
        "写出后进程退出，没等到回显".into()
    )));
    let snapshot = h
        .wait(&session, "partial", |s| {
            s.items
                .iter()
                .any(|i| i.id == "header" && i.data["status"] == "partial")
        })
        .await;
    let irreversible = header(&snapshot)["irreversible"].to_string();
    assert!(irreversible.contains("first"), "{irreversible}");
    assert!(irreversible.contains("可能已做"), "{irreversible}");
    assert_eq!(prompt(&snapshot, "你好").unwrap().data["state"], "unknown");
    // 补偿照常做：本次新建的后端进程被弃置；会话本身留着，标「部分完成」。
    assert!(h.adapter.received().iter().any(|(_, a)| matches!(
        a,
        Act::End {
            how: nd_backend::EndHow::Discard,
            ..
        }
    )));
    let listed = h.sessions.listing().items();
    let entry = listed
        .iter()
        .find(|i| i.id == format!("session/{session}"))
        .expect("partial session stays listed");
    assert_eq!(entry.data["status"], "partial");
    assert_eq!(
        prompt(&snapshot, "during creation").unwrap().data["state"],
        "failed"
    );
    let reply = h.send("after-partial", &session, "no carrier").await;
    assert!(matches!(reply, nd_wire::CommandReply::Receipt {
        receipt: nd_wire::Receipt::Rejected { ref code, .. }
    } if code == "precondition"));
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_sent_while_the_session_is_being_created_are_held_then_sent_in_order() {
    let h = Harness::new(config()).await;
    h.adapter.script(
        nd_session::scripted::ActKind::Open,
        nd_session::scripted::Reply::Hold,
    );
    let session = accepted_session(&h.create("create-held", "第一条").await);
    h.send("m-1", &session, "第二条").await;
    h.send("m-2", &session, "第三条").await;
    let snapshot = h
        .wait(&session, "held prompts", |s| {
            ["第二条", "第三条"]
                .iter()
                .all(|t| prompt(s, t).is_some_and(|p| p.data["state"] == "held"))
        })
        .await;
    assert_eq!(header(&snapshot)["status"], "preparing");
    assert!(h.adapter.release(nd_session::scripted::Reply::Ok));
    h.wait(&session, "all landed", |s| {
        ["第一条", "第二条", "第三条"]
            .iter()
            .all(|t| prompt(s, t).is_some_and(|p| p.data["state"] == "landed"))
    })
    .await;
    let sends: Vec<String> = h
        .adapter
        .applied()
        .into_iter()
        .filter(|a| a.starts_with("send:"))
        .collect();
    assert_eq!(sends, ["send:第一条", "send:第二条", "send:第三条"]);
}

fn idle(ms: u64) -> nd_session::EngineConfig {
    nd_session::EngineConfig {
        idle_reclaim: std::time::Duration::from_millis(ms),
        ..config()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_process_is_reclaimed_and_the_next_message_launches_it_again() {
    let h = Harness::new(idle(150)).await;
    let session = accepted_session(&h.create("create-idle", "你好").await);
    h.wait(&session, "reclaimed", |s| {
        s.items.iter().any(|i| {
            i.id == "header" && i.data["status"] == "active" && i.data["process"]["alive"] == false
        })
    })
    .await;
    assert!(h.adapter.applied().iter().any(|a| a.starts_with("end:")));
    assert!(h.adapter.live().is_empty());
    h.send("after-idle", &session, "又来了").await;
    h.wait(&session, "landed after relaunch", |s| {
        prompt(s, "又来了").is_some_and(|p| p.data["state"] == "landed")
    })
    .await;
    let opens: Vec<_> = h
        .adapter
        .received()
        .into_iter()
        .filter_map(|(_, a)| match a {
            Act::Open { spec, run, .. } => Some((spec.origin, run)),
            _ => None,
        })
        .collect();
    assert_eq!(opens.len(), 2, "{opens:?}");
    assert!(matches!(opens[0].0, nd_backend::Origin::Fresh { .. }));
    // 续接同一个后端会话，用新的后端进程编号。
    assert!(
        matches!(&opens[1].0, nd_backend::Origin::Resume { bs } if bs == opens[0].0.backend_session())
    );
    assert_ne!(opens[0].1, opens[1].1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_with_busy_or_unknown_background_work_is_not_reclaimed() {
    for drain in [
        nd_backend::Drain::Busy,
        nd_backend::Drain::Unknown {
            why: "拿不到任务表".into(),
        },
    ] {
        let h = Harness::new(idle(50)).await;
        h.adapter.set_drain(drain.clone());
        let session = accepted_session(&h.create("create-busy", "你好").await);
        h.wait(&session, "active", |s| header(s)["status"] == "active")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(
            !h.adapter
                .received()
                .iter()
                .any(|(_, a)| matches!(a, Act::End { .. })),
            "{drain:?} must not be reclaimed"
        );
        assert_eq!(h.adapter.live().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watched_session_is_not_reclaimed_until_nobody_is_watching() {
    let h = Harness::new(idle(50)).await;
    let session = accepted_session(&h.create("create-watched", "你好").await);
    let watching = h.sessions.subscribe(&session, None).unwrap().unwrap();
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(h.adapter.live().len(), 1, "watched process kept");
    drop(watching);
    h.wait(&session, "reclaimed", |s| {
        header(s)["process"]["alive"] == false
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_delivery_survives_restart_and_only_confirmed_loss_allows_one_user_resend() {
    use nd_backend::{Outcome, Refusal};
    use nd_session::scripted::{ActKind, Reply};
    use nd_wire::{CommandReply, Receipt};
    use serde_json::json;
    let h = Harness::new(config()).await;
    let session = accepted_session(&h.create("clarify-create", "开始").await);
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    h.adapter
        .script(ActKind::Send, Reply::Unknown("连接中断".into()));
    let original = command(
        "uncertain",
        "session.send",
        json!({"session":session,"text":"不能盲重发"}),
    );
    let receipt = h.sessions.execute(&original).await.unwrap();
    h.wait(&session, "unknown", |s| {
        prompt(s, "不能盲重发").is_some_and(|i| i.data["state"] == "unknown")
    })
    .await;
    let resend = |id| {
        command(
            id,
            "session.resend",
            json!({"session":session,"message":"uncertain"}),
        )
    };
    assert!(matches!(
        h.sessions.execute(&resend("too-early")).await,
        Some(CommandReply::Receipt {
            receipt: Receipt::Rejected { .. }
        })
    ));
    let ticket = h.adapter.received().last().unwrap().0.clone();
    let h = h.restart(config()).await;
    h.wait(&session, "recovered", |s| header(s)["recovering"] == false)
        .await;
    assert_eq!(h.sessions.execute(&original).await.unwrap(), receipt);
    assert_eq!(
        h.adapter
            .received()
            .iter()
            .filter(|(_, a)| matches!(a, Act::Send { msg, .. } if msg.text == "不能盲重发"))
            .count(),
        1
    );
    h.adapter.clarify(
        &ticket,
        Outcome::Refused {
            refusal: Refusal::Lost {
                evidence: "后端明确拒绝，未消费".into(),
            },
        },
    );
    h.wait(&session, "confirmed loss", |s| {
        prompt(s, "不能盲重发").is_some_and(|i| i.data["state"] == "not_delivered")
    })
    .await;
    assert_eq!(
        h.adapter
            .received()
            .iter()
            .filter(|(_, a)| matches!(a, Act::Send { msg, .. } if msg.text == "不能盲重发"))
            .count(),
        1
    );
    let h = h.restart(config()).await;
    h.wait(&session, "recovered loss", |s| {
        header(s)["recovering"] == false
    })
    .await;
    let sent = h.sessions.execute(&resend("resend-once")).await.unwrap();
    assert!(matches!(
        sent,
        CommandReply::Receipt {
            receipt: Receipt::Done { .. }
        }
    ));
    assert_eq!(
        h.sessions.execute(&resend("resend-once")).await.unwrap(),
        sent
    );
    assert!(matches!(
        h.sessions.execute(&resend("other-device")).await,
        Some(CommandReply::Receipt {
            receipt: Receipt::Rejected { .. }
        })
    ));
    h.wait(&session, "resend lands", |s| {
        s.items
            .iter()
            .any(|i| i.data["message"] == "resend-once" && i.data["state"] == "landed")
    })
    .await;
    assert_eq!(h.sessions.execute(&original).await.unwrap(), receipt);
    assert_eq!(
        h.adapter
            .applied()
            .iter()
            .filter(|s| *s == "send:不能盲重发")
            .count(),
        1
    );
}

/// 收据等动作有结果：结果出来之前同 id 同内容的重试等同一个结果，不同内容回 conflict；
/// 结果到了两个等待者拿到同一张收据，之后再查也是它，动作只执行一次。
#[tokio::test(flavor = "multi_thread")]
async fn a_pending_bang_answers_every_identical_retry_with_one_receipt() {
    use nd_session::scripted::{ActKind, Reply};
    let h = std::sync::Arc::new(Harness::new(config()).await);
    let session = accepted_session(&h.create("bang-create", "你好").await);
    h.wait(&session, "active", |s| header(s)["status"] == "active")
        .await;
    h.adapter.script(ActKind::Invoke, Reply::Hold);
    let bang = command(
        "bang-1",
        "session.shell",
        serde_json::json!({"session":session,"command":"pwd"}),
    );
    let first = {
        let (h, bang) = (h.clone(), bang.clone());
        tokio::spawn(async move { h.sessions.execute(&bang).await.unwrap() })
    };
    h.wait(&session, "the bang was handed to the backend", |s| {
        s.items
            .iter()
            .any(|i| i.id == "invoke/bang-1" && i.data["state"] == "pending")
    })
    .await;
    let mut changed = bang.clone();
    changed.args["command"] = serde_json::json!("ls");
    assert_eq!(
        h.sessions.execute(&changed).await,
        Some(nd_wire::CommandReply::Conflict)
    );
    let second = {
        let (h, bang) = (h.clone(), bang.clone());
        tokio::spawn(async move { h.sessions.execute(&bang).await.unwrap() })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!first.is_finished() && !second.is_finished());
    assert!(h.adapter.release(Reply::Ok));
    let (first, second) = (first.await.unwrap(), second.await.unwrap());
    assert_eq!(first, second);
    let nd_wire::CommandReply::Receipt {
        receipt: nd_wire::Receipt::Done { value },
    } = &first
    else {
        panic!("{first:?}");
    };
    assert_eq!(value["invoke"], "bang-1");
    assert_eq!(value["exit"], 0);
    assert_eq!(h.sessions.execute(&bang).await, Some(first));
    assert_eq!(
        h.adapter
            .applied()
            .iter()
            .filter(|s| *s == "invoke:shell:pwd")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reclaim_does_not_promote_a_model_default_effort_into_a_user_override() {
    let mut cfg = config();
    cfg.idle_reclaim = std::time::Duration::from_millis(60);
    let h = Harness::new(cfg).await;
    h.adapter.set_initial_settings(
        serde_json::from_value(serde_json::json!({"applied":{"effort":"medium"}})).unwrap(),
    );
    let session = accepted_session(&h.create("default-effort", "first").await);
    h.wait(&session, "reclaimed", |s| {
        header(s)["status"] == "active" && header(s)["process"]["alive"] == false
    })
    .await;
    h.send("after-reclaim", &session, "next").await;
    h.wait(&session, "resumed", |s| {
        prompt(s, "next").is_some_and(|p| p.data["state"] == "landed")
    })
    .await;
    let opened: Vec<_> = h
        .adapter
        .received()
        .into_iter()
        .filter_map(|(_, act)| match act {
            Act::Open { spec, .. } => Some(spec.profile.effort),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened,
        [None, None],
        "only a confirmed user choice may be replayed as an effort override"
    );
}
