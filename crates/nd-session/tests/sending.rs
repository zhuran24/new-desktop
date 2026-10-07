//! 窄接缝一：发送台在后台非结构操作进行中仍允许其他输入。
mod support;
use nd_session::scripted::{ActKind, Reply};
use serde_json::json;
use std::time::Duration;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_automatic_title_does_not_hold_shell_invocations() {
    let mut cfg = config();
    cfg.auto_title = true;
    let h = Harness::new(cfg).await;
    h.adapter.script(ActKind::Title, Reply::Hold);
    let session = accepted_session(
        &h.create("title-hold", "足够长的首条提示会在首轮结束后自动生成标题")
            .await,
    );
    h.wait(&session, "title pending", |s| {
        header(s)["status"] == "active"
            && s.items
                .iter()
                .any(|i| i.kind == "op" && i.data["kind"] == "title")
    })
    .await;
    let command = command(
        "shell-during-title",
        "session.shell",
        json!({"session":session,"command":"pwd"}),
    );
    let reply = tokio::time::timeout(Duration::from_millis(500), h.sessions.execute(&command))
        .await
        .expect("nonstructural title must not block the sending queue")
        .unwrap();
    assert!(matches!(
        reply,
        nd_wire::CommandReply::Receipt {
            receipt: nd_wire::Receipt::Done { .. }
        }
    ));
    assert!(h.adapter.release(Reply::Ok));
}
