//! 纯投影接缝：小输入验证游标边界与谱系过滤；千轮规模由主接缝验证。
use nd_session::history::History;
use nd_wire::{Fallback, Item, PageReq};
use serde_json::json;

#[test]
fn queued_messages_and_pending_controls_stay_visible_outside_the_body_window() {
    let mut history = History::new("session/queue".into());
    history.insert(item(
        "prompt/queued",
        "prompt",
        1,
        json!({"message":"queued","state":"withdrawing","text":"return me"}),
    ));
    history.insert(item(
        "control/esc",
        "control",
        2,
        json!({"state":"pending"}),
    ));
    for n in 3..103 {
        history.insert(item(
            &format!("block/{n}"),
            "text",
            n,
            json!({"text":"body","complete":true}),
        ));
    }
    let snapshot = history.snapshot();
    assert!(snapshot.iter().any(|i| i.id == "prompt/queued"));
    assert!(snapshot.iter().any(|i| i.id == "control/esc"));
    history.insert(item(
        "prompt/queued",
        "prompt",
        1,
        json!({"message":"queued","state":"withdrawn","text":"return me"}),
    ));
    history.insert(item(
        "control/esc",
        "control",
        2,
        json!({"state":"acknowledged"}),
    ));
    assert!(
        !history
            .snapshot()
            .iter()
            .any(|i| matches!(i.id.as_str(), "prompt/queued" | "control/esc"))
    );
}

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
fn four_rounds() -> History {
    let mut history = History::new("session/long".into());
    let mut rounds = vec![];
    for n in 1..=4 {
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
            json!({"text":"回答","complete":true}),
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
fn around_and_after_pages_preserve_boundaries_at_the_first_and_last_round() {
    let history = four_rounds();
    let page = history
        .page(&PageReq {
            around: Some("r2".into()),
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.anchor.as_deref(), Some("prompt/p2"));
    let after = history
        .page(&PageReq {
            after: page.newer,
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(after.items[0].id, "prompt/p3");
    let first = history
        .page(&PageReq {
            around: Some("r1".into()),
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert!(first.next.is_none());
    let last = history
        .page(&PageReq {
            around: Some("r4".into()),
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert!(last.newer.is_none());
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
    use nd_backend::BackendSessionId;
    use nd_session::lineage::{BranchKind, Event, Lineage, NativePosition};
    let mut lineage = Lineage::default()
        .fold(&Event::Root {
            segment: "old".into(),
            carrier: "c1".into(),
            backend_session: BackendSessionId::claude("bs1"),
        })
        .unwrap();
    for (id, key) in [("a", "merged"), ("b", "merged"), ("discarded", "discarded")] {
        lineage = lineage
            .fold(&Event::Landed {
                message: id.into(),
                ticket: id.into(),
                position: NativePosition {
                    carrier: "c1".into(),
                    backend_session: BackendSessionId::claude("bs1"),
                    native: id.into(),
                },
            })
            .unwrap();
        if id != "a" {
            lineage = lineage
                .fold(&Event::TurnObserved {
                    carrier: "c1".into(),
                    backend_session: BackendSessionId::claude("bs1"),
                    key: key.into(),
                    natives: if id == "b" {
                        vec!["a".into(), "b".into()]
                    } else {
                        vec![id.into()]
                    },
                    complete: true,
                    last_assistant: None,
                })
                .unwrap();
        }
    }
    let merged = lineage.rounds("old").unwrap()[0].id.clone();
    lineage = lineage
        .fold(&Event::Branch {
            segment: "new".into(),
            from: "old".into(),
            through: Some(merged.clone()),
            kind: BranchKind::Rewind,
            carrier: "c2".into(),
            backend_session: BackendSessionId::claude("bs2"),
        })
        .unwrap();
    lineage = lineage
        .fold(&Event::Landed {
            message: "c".into(),
            ticket: "c".into(),
            position: NativePosition {
                carrier: "c2".into(),
                backend_session: BackendSessionId::claude("bs2"),
                native: "c".into(),
            },
        })
        .unwrap();
    lineage = lineage
        .fold(&Event::TurnObserved {
            carrier: "c2".into(),
            backend_session: BackendSessionId::claude("bs2"),
            key: "last".into(),
            natives: vec!["c".into()],
            complete: true,
            last_assistant: None,
        })
        .unwrap();
    let rounds = serde_json::to_value(lineage.rounds("new").unwrap()).unwrap();
    history.insert(item(
        "lineage",
        "lineage",
        1,
        json!({"current":"new","rounds":rounds,"inactive_messages":lineage.inactive_messages()}),
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
            around: Some(merged),
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
