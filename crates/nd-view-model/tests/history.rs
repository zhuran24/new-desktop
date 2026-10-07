use nd_view_model::{HistoryView, conversation};
use nd_wire::{Fallback, Item, Page, Snapshot};
use serde_json::json;
fn snapshot(stream: &str, segment: &str) -> Snapshot {
    Snapshot {
        stream: stream.into(),
        epoch: "e".into(),
        cursor: 1,
        items: vec![
            Item {
                id: "navigation".into(),
                namespace: "session".into(),
                kind: "navigation".into(),
                data: json!({"segment":segment,"rounds":[{"id":"r1","n":1,"preview":"你好","anchor":"prompt/old"}]}),
                fallback: Fallback {
                    title: "导航".into(),
                    text: String::new(),
                },
            },
            Item {
                id: "history".into(),
                namespace: "session".into(),
                kind: "history".into(),
                data: json!({"older":"cursor"}),
                fallback: Fallback {
                    title: "历史".into(),
                    text: String::new(),
                },
            },
            Item {
                id: "prompt/new".into(),
                namespace: "session".into(),
                kind: "prompt".into(),
                data: json!({"text":"最新","seq":10}),
                fallback: Fallback {
                    title: "你".into(),
                    text: String::new(),
                },
            },
        ],
    }
}
#[test]
fn navigation_loads_a_missing_round_and_ignores_late_pages_after_switching_sessions() {
    let mut view = HistoryView::default();
    view.observe(snapshot("session/a", "root"));
    assert_eq!(view.rounds()[0].preview, "你好");
    assert_eq!(
        conversation(&view.snapshot().unwrap()).messages.len(),
        1,
        "metadata is not chat text"
    );
    let old = view.request();
    let latest = view.request();
    let page = Page {
        items: vec![Item {
            id: "prompt/old".into(),
            namespace: "session".into(),
            kind: "prompt".into(),
            data: json!({"text":"旧提示","seq":1}),
            fallback: Fallback {
                title: "你".into(),
                text: String::new(),
            },
        }],
        next: None,
        newer: Some("newer".into()),
        anchor: Some("prompt/old".into()),
        at: None,
    };
    assert!(!view.loaded(old, Ok(page.clone())));
    assert!(view.loaded(latest, Ok(page.clone())));
    assert_eq!(
        conversation(&view.snapshot().unwrap()).messages[0].text,
        "旧提示"
    );
    view.observe(snapshot("session/a", "root"));
    assert_eq!(
        conversation(&view.snapshot().unwrap()).messages[0].text,
        "旧提示",
        "live tail cannot pull the reader away"
    );
    let pending = view.request();
    view.observe(snapshot("session/b", "root"));
    assert!(!view.loaded(pending, Ok(page)));
    assert_eq!(
        conversation(&view.snapshot().unwrap()).messages[0].text,
        "最新"
    );
    assert_eq!(view.older(), Some("cursor".into()));
}

#[test]
fn a_page_reply_cannot_roll_back_streaming_content_received_while_it_was_loading() {
    let mut view = HistoryView::default();
    let mut live = snapshot("session/a", "root");
    live.items[2].kind = "text".into();
    live.items[2].data = json!({"text":"片段","complete":false,"seq":10});
    view.observe(live.clone());
    let generation = view.request();
    let page = Page {
        items: vec![live.items[2].clone()],
        next: None,
        newer: None,
        anchor: None,
        at: Some(nd_wire::Cursor {
            epoch: "e".into(),
            seq: 1,
        }),
    };
    live.items[2].data["text"] = json!("片段和后续");
    live.cursor = 2;
    view.observe(live);
    view.loaded(generation, Ok(page));
    assert_eq!(
        conversation(&view.snapshot().unwrap()).messages[0].text,
        "片段和后续"
    );
}

#[test]
fn a_lagging_subscription_cannot_overwrite_a_newer_history_page() {
    let mut view = HistoryView::default();
    let mut live = snapshot("session/a", "root");
    view.observe(live.clone());
    let token = view.request();
    let mut current = live.items[2].clone();
    current.data["text"] = json!("查询已看到的终稿");
    view.loaded(
        token,
        Ok(Page {
            items: vec![current],
            next: None,
            newer: None,
            anchor: None,
            at: Some(nd_wire::Cursor {
                epoch: "e".into(),
                seq: 5,
            }),
        }),
    );
    live.cursor = 2;
    view.observe(live);
    assert_eq!(
        conversation(&view.snapshot().unwrap()).messages[0].text,
        "查询已看到的终稿"
    );
}

#[test]
fn reconnect_keeps_the_reading_page_but_rejects_a_reply_from_the_old_connection() {
    let mut view = HistoryView::default();
    view.observe(snapshot("session/a", "root"));
    let token = view.request();
    let page = Page {
        items: vec![],
        next: None,
        newer: None,
        anchor: Some("old-anchor".into()),
        at: Some(nd_wire::Cursor {
            epoch: "e".into(),
            seq: 1,
        }),
    };
    view.loaded(token, Ok(page.clone()));
    let pending = view.request();
    let mut fresh = snapshot("session/a", "root");
    fresh.epoch = "reopened".into();
    view.observe(fresh);
    assert!(!view.loaded(pending, Ok(page)));
    assert_eq!(view.anchor(), Some("old-anchor"));
}
