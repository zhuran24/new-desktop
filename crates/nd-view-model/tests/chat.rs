use nd_view_model::{ViewState, sidebar};
use nd_wire::{Fallback, Item, Snapshot};
use serde_json::json;

#[test]
fn an_unconfirmed_send_keeps_its_text_until_the_user_edits_or_retries_saving() {
    let mut editor = nd_view_model::Draft::default();
    editor.observe(
        nd_wire::Draft {
            version: 1,
            text: "交付不明的正文".into(),
            ..Default::default()
        },
        false,
    );
    editor.unconfirmed_send();
    editor.observe(
        nd_wire::Draft {
            version: 2,
            ..Default::default()
        },
        false,
    );
    assert_eq!(editor.text(), "交付不明的正文");
    assert!(
        editor.save_command("s", "a", "automatic").is_none(),
        "a snapshot must not silently overwrite or resave uncertain text"
    );
    editor.retry_save();
    let command = editor.save_command("s", "a", "explicit").unwrap();
    assert_eq!(command.expect["draft_version"], 1);
    assert_eq!(command.args["text"], "交付不明的正文");
}

#[test]
fn draft_sync_preserves_new_edits_while_an_older_save_is_in_flight() {
    use nd_view_model::Draft;
    let remote = |version, text: &str| nd_wire::Draft {
        version,
        text: text.into(),
        ..Default::default()
    };
    let mut editor = Draft::default();
    editor.observe(remote(1, "原稿"), false);
    assert_eq!(editor.text(), "原稿");
    editor.edit("第一改".into());
    let save = editor.save_command("s", "a", "save-1").unwrap();
    editor.edit("第二改".into());
    editor.observe(remote(2, "第一改"), false);
    assert_eq!(editor.text(), "第二改");
    assert_eq!(
        editor.save_command("s", "a", "unused"),
        Some(save.clone()),
        "uncertain save retries use exactly the same id and body"
    );
    editor.saved(
        nd_wire::DraftUpdated {
            draft: remote(2, "第一改"),
            saved: None,
        },
        false,
    );
    assert_eq!(editor.text(), "第二改");
    let next = editor.save_command("s", "a", "save-2").unwrap();
    assert_eq!(next.expect["draft_version"], 2);
    assert_eq!(next.args["text"], "第二改");
    editor.saved(
        nd_wire::DraftUpdated {
            draft: remote(3, "第二改"),
            saved: None,
        },
        false,
    );
    assert!(editor.is_saved());
    editor.observe(remote(4, "另一台"), false);
    assert_eq!(editor.text(), "另一台");
}

#[test]
fn draft_remote_updates_defer_to_composition_and_a_sent_reply_keeps_new_edits() {
    let remote = |version, text: &str| nd_wire::Draft {
        version,
        text: text.into(),
        ..Default::default()
    };
    let mut editor = nd_view_model::Draft::default();
    editor.observe(remote(1, "原稿"), false);
    editor.observe(remote(2, "另一台"), true);
    assert_eq!(editor.text(), "原稿");
    editor.edit("组词确认后的内容".into());
    let command = editor.save_command("s", "a", "conflict").unwrap();
    assert_eq!(command.expect["draft_version"], 1);
    editor.saved(
        nd_wire::DraftUpdated {
            draft: remote(2, "另一台"),
            saved: Some("conflict".into()),
        },
        false,
    );
    assert_eq!(editor.text(), "另一台");
    let sent = editor.revision();
    editor.edit("发送后继续输入".into());
    editor.sent(sent, remote(3, ""), false);
    assert_eq!(editor.text(), "发送后继续输入");
    assert_eq!(
        editor.save_command("s", "a", "next").unwrap().expect["draft_version"],
        3
    );
    editor.saved(
        nd_wire::DraftUpdated {
            draft: remote(4, "发送后继续输入"),
            saved: None,
        },
        false,
    );
    editor.sent(editor.revision(), remote(5, ""), true);
    assert_eq!(
        editor.text(),
        "发送后继续输入",
        "an in-progress composition is protected"
    );
    editor.observe(remote(5, ""), false);
    assert_eq!(editor.text(), "");
}

fn item(id: &str, kind: &str, data: serde_json::Value) -> Item {
    Item {
        id: id.into(),
        namespace: "sessions".into(),
        kind: kind.into(),
        data,
        fallback: Fallback {
            title: "项目".into(),
            text: "失败原因".into(),
        },
    }
}

#[test]
fn sidebar_keeps_preparing_partial_and_withdrawn_notices_visible() {
    let snapshot = Snapshot {
        stream: "global".into(),
        epoch: "e".into(),
        cursor: 0,
        items: vec![
            item(
                "session/a",
                "session",
                json!({"session":"a","status":"preparing","cwd":"/work/a","model":"chosen"}),
            ),
            item(
                "session/b",
                "session",
                json!({"session":"b","status":"partial","cwd":"/work/b","note":"首条可能已发"}),
            ),
            item(
                "notice/c",
                "notice",
                json!({"status":"withdrawn","reason":"目录不存在"}),
            ),
            item("system", "system", json!({})),
        ],
    };
    let state = ViewState {
        selected_session: Some("b".into()),
        ..Default::default()
    };
    let rows = sidebar(&snapshot, &state);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].status, "准备中");
    assert_eq!(rows[0].model, "chosen");
    assert_eq!(rows[1].status, "部分完成");
    assert_eq!(rows[1].detail, "首条可能已发");
    assert!(rows[1].selected);
    assert_eq!(rows[2].status, "创建失败");
    assert_eq!(rows[2].detail, "目录不存在");
    assert_eq!(rows[2].session, None);
}

#[test]
fn conversation_orders_by_sequence_and_replaces_the_streaming_block() {
    let mut snapshot = Snapshot {
        stream: "session/a".into(),
        epoch: "e".into(),
        cursor: 2,
        items: vec![
            item(
                "block/1",
                "text",
                json!({"seq":2,"text":"# 中文\n```rust\nfn main()", "complete":false}),
            ),
            item("header", "header", json!({"status":"active"})),
            item(
                "prompt/1",
                "prompt",
                json!({"seq":1,"text":"你好", "state":"unknown"}),
            ),
            item("future/1", "future", json!({"seq":3})),
        ],
    };
    let view = nd_view_model::conversation(&snapshot);
    assert!(view.can_send);
    assert_eq!(
        view.messages
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["prompt/1", "block/1", "future/1"]
    );
    assert_eq!(view.messages[0].status, "交付不明");
    assert!(view.messages[1].markdown);
    assert_eq!(view.messages[1].status, "生成中");
    assert_eq!(view.messages[2].text, "失败原因");
    snapshot.items[0].data =
        json!({"seq":2,"text":"# 中文\n```rust\nfn main() {}\n```", "complete":true});
    let view = nd_view_model::conversation(&snapshot);
    assert_eq!(view.messages.len(), 3);
    assert_eq!(view.messages[1].text, "# 中文\n```rust\nfn main() {}\n```");
    assert_eq!(view.messages[1].status, "");
    snapshot.items[1].data = json!({"status":"partial", "note":"创建未完成"});
    assert!(!nd_view_model::conversation(&snapshot).can_send);
}

#[test]
fn an_accepted_send_clears_only_the_submitted_draft_revision() {
    use nd_view_model::Draft;
    let mut draft = Draft::default();
    draft.edit("第一条".into());
    let submitted = draft.revision();
    draft.edit("第二条".into());
    assert!(!draft.accept(submitted));
    assert_eq!(draft.text(), "第二条");
    assert!(draft.accept(draft.revision()));
    assert_eq!(draft.text(), "");
    use nd_wire::{CommandReply, Receipt};
    assert!(!nd_view_model::accepted(&CommandReply::DeliveryUnknown));
    assert!(!nd_view_model::accepted(&CommandReply::Receipt {
        receipt: Receipt::Rejected {
            code: "invalid".into(),
            now: json!({})
        }
    }));
}

#[test]
fn completed_creation_is_out_of_the_chat_and_partial_failure_stays_explained() {
    let mut s = Snapshot {
        stream: "session/a".into(),
        epoch: "e".into(),
        cursor: 0,
        items: vec![
            item(
                "header",
                "header",
                json!({"status":"active", "cwd":"/work", "model":"haiku"}),
            ),
            item(
                "op/create",
                "op",
                json!({"seq":1,"kind":"create","phase":"done"}),
            ),
        ],
    };
    let view = nd_view_model::conversation(&s);
    assert_eq!(view.header, "可对话 · /work · haiku");
    assert!(view.messages.is_empty());
    s.items[0].data =
        json!({"status":"partial","cwd":"/work","model":"haiku","note":"首条消息交付不明"});
    s.items[1].data =
        json!({"seq":1,"kind":"create","phase":"partial","reason":"首条消息交付不明"});
    let view = nd_view_model::conversation(&s);
    assert!(view.header.contains("部分完成"));
    assert_eq!(view.messages[0].title, "新建会话");
    assert_eq!(view.messages[0].text, "首条消息交付不明");
}

#[test]
fn delivery_unknown_is_explained_and_only_confirmed_non_delivery_offers_resend() {
    let snapshot = Snapshot {
        stream: "session/s".into(),
        epoch: "e".into(),
        cursor: 0,
        items: vec![
            item(
                "prompt/u",
                "prompt",
                json!({"message":"u","state":"unknown","text":"一条提示","reason":"还没有原 uuid 的回显"}),
            ),
            item(
                "prompt/l",
                "prompt",
                json!({"message":"l","state":"not_delivered","text":"另一条提示","reason":"CLI 明确拒绝"}),
            ),
            item(
                "prompt/r",
                "prompt",
                json!({"message":"r","state":"resent","text":"旧提示"}),
            ),
        ],
    };
    let view = nd_view_model::conversation(&snapshot);
    assert_eq!(view.messages[0].status, "交付不明");
    assert_eq!(view.messages[0].detail, "还没有原 uuid 的回显");
    assert_eq!(view.messages[0].resend, None);
    assert_eq!(view.messages[1].status, "未送达");
    assert_eq!(view.messages[1].resend.as_deref(), Some("l"));
    assert_eq!(view.messages[2].resend, None);
}

#[test]
fn attachment_edits_participate_in_draft_receipt_revision() {
    let mut draft = nd_view_model::Draft::default();
    let a = nd_wire::Attachment {
        blob: "a".repeat(64),
        name: "图.png".into(),
        media_type: "image/png".into(),
        size: 42,
    };
    draft.attach(a.clone());
    let submitted = draft.revision();
    draft.detach(0);
    assert!(!draft.accept(submitted));
    draft.attach(a.clone());
    assert_eq!(draft.attachments(), &[a]);
    assert!(draft.accept(draft.revision()));
    assert!(draft.attachments().is_empty());
}
