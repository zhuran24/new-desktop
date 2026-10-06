use nd_view_model::{ViewState, sidebar};
use nd_wire::{Fallback, Item, Snapshot};
use serde_json::json;

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
