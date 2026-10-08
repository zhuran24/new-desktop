//! 窄接缝一：经名册的命令和会话流驱动真的会话组件、独占登记与 SQLite，后端是脚本化适配器。
mod support;
use nd_backend::Act;
use support::*;

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
