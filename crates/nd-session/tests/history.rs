//! 纯投影接缝：规模、游标和导航均由公开显示历史接口观察。
use nd_session::history::History;
use nd_wire::{Fallback, Item, PageReq};
use serde_json::json;

fn item(id: &str, kind: &str, seq: u64, data: serde_json::Value) -> Item {
    let mut data = data;
    data["seq"] = json!(seq);
    Item {
        id: id.into(),
        namespace: "session".into(),
        kind: kind.into(),
        data,
        fallback: Fallback {
            title: kind.into(),
            text: String::new(),
        },
    }
}
fn thousand() -> History {
    let mut history = History::new("session/long".into());
    let mut rounds = vec![];
    for n in 1..=1000 {
        history.insert(item(
            &format!("prompt/p{n}"),
            "prompt",
            n * 2,
            json!({"text":format!("第 {n} 轮提示"),"message":format!("p{n}")}),
        ));
        history.insert(item(
            &format!("block/a{n}"),
            "text",
            n * 2 + 1,
            json!({"text":"长回答".repeat(1000),"complete":true}),
        ));
        rounds
            .push(json!({"id":format!("r{n}"),"n":n,"messages":[format!("p{n}")],"complete":true}));
    }
    history.insert(item(
        "lineage",
        "lineage",
        1,
        json!({"current":"root","rounds":rounds}),
    ));
    history
}
#[test]
fn a_thousand_rounds_open_as_one_bounded_page_with_all_navigation_marks() {
    let history = thousand();
    let snapshot = history.snapshot();
    assert_eq!(snapshot.iter().filter(|i| i.kind == "text").count(), 30);
    assert_eq!(snapshot.iter().filter(|i| i.kind == "prompt").count(), 30);
    let nav = snapshot.iter().find(|i| i.kind == "navigation").unwrap();
    assert_eq!(nav.data["rounds"].as_array().unwrap().len(), 1000);
    assert_eq!(nav.data["rounds"][0]["preview"], "第 1 轮提示");
    assert_eq!(nav.data["rounds"][999]["n"], 1000);
    let page = history
        .page(&PageReq {
            around: Some("r500".into()),
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.anchor.as_deref(), Some("prompt/p500"));
    assert_eq!(
        page.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["prompt/p500", "block/a500"]
    );
    assert!(page.next.is_some());
    assert!(page.newer.is_some());
    let next = history
        .page(&PageReq {
            after: page.newer,
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(next.items[0].id, "prompt/p501");
}

#[test]
fn merged_prompts_share_one_mark_and_old_branch_cursors_cannot_jump_into_a_new_segment() {
    let mut history = History::new("session/branches".into());
    for (seq, id, text) in [
        (2, "a", "压缩前的提示"),
        (4, "b", "一起发送 🦀"),
        (6, "discarded", "旁支提示"),
        (8, "c", "压缩后的提示"),
    ] {
        history.insert(item(
            &format!("prompt/{id}"),
            "prompt",
            seq,
            json!({"message":id,"text":text,"state":"landed"}),
        ));
        history.insert(item(
            &format!("block/{id}"),
            "text",
            seq + 1,
            json!({"text":text,"complete":true}),
        ));
    }
    let rounds = json!([
        {"id":"merged","n":1,"messages":["a","b"],"complete":true},
        {"id":"last","n":2,"messages":["c"],"complete":true}
    ]);
    history.insert(item(
        "lineage",
        "lineage",
        1,
        json!({"current":"new","rounds":rounds,"inactive_messages":["discarded"]}),
    ));
    assert_eq!(
        history.navigation().data["rounds"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let page = history
        .page(&PageReq {
            around: Some("merged".into()),
            limit: 4,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        page.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["prompt/a", "block/a", "prompt/b", "block/b"]
    );
    let after = history
        .page(&PageReq {
            after: page.newer.clone(),
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        after.items[0].id, "prompt/c",
        "cut-off branch must not leak into current history"
    );
    assert!(
        history.navigation().data["rounds"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("🦀")
    );
    history.insert(item(
        "lineage",
        "lineage",
        1,
        json!({"current":"other","rounds":rounds}),
    ));
    assert!(
        history
            .page(&PageReq {
                after: page.newer,
                ..Default::default()
            })
            .is_err()
    );
    assert!(
        history
            .page(&PageReq {
                around: Some("not-a-round".into()),
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn walking_pages_matches_the_simple_projection_and_cursors_survive_append() {
    use nd_session::projection::{Shown, project};
    let log: Vec<_> = (0..137)
        .map(|n| Shown::Prompt {
            id: format!("p{n}"),
            text: format!("提示 {n}"),
            intent: "fold".into(),
            state: "landed".into(),
            native: None,
            reason: None,
            attachments: vec![],
        })
        .collect();
    let expected = project(&log);
    let mut history = History::new("session/pages".into());
    for item in &expected {
        history.insert(item.clone());
    }
    let mut request = PageReq {
        limit: 17,
        ..Default::default()
    };
    let mut actual = vec![];
    loop {
        let page = history.page(&request).unwrap();
        actual.splice(0..0, page.items);
        request.before = page.next;
        if request.before.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let page = history
        .page(&PageReq {
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    history.insert(item(
        "prompt/later",
        "prompt",
        1000,
        json!({"text":"稍后到达","message":"later"}),
    ));
    let earlier = history
        .page(&PageReq {
            before: page.next,
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(earlier.items[0].data["text"], "提示 135");
    assert!(
        history
            .page(&PageReq {
                limit: 0,
                ..Default::default()
            })
            .is_err()
    );
    assert!(
        history
            .page(&PageReq {
                limit: 101,
                ..Default::default()
            })
            .is_err()
    );
    let cursor = earlier.next.unwrap();
    let mut other = History::new("session/other".into());
    for item in expected {
        other.insert(item);
    }
    assert!(
        other
            .page(&PageReq {
                before: Some(cursor),
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn attachment_only_rounds_have_a_readable_preview_and_a_jump_anchor() {
    let mut history = History::new("session/images".into());
    history.insert(item(
        "prompt/image",
        "prompt",
        2,
        json!({"message":"image","text":"","attachments":[{"name":"截图.png"}]}),
    ));
    history.insert(item(
        "lineage",
        "lineage",
        1,
        json!({"current":"root","rounds":[{"id":"r-image","n":1,"messages":["image"]}]}),
    ));
    let navigation = history.navigation();
    assert_eq!(navigation.data["rounds"][0]["preview"], "附件：截图.png");
    assert_eq!(navigation.data["rounds"][0]["anchor"], "prompt/image");
}
