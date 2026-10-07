//! 总结、`!` 模式、fork 型子代理与降级提示的界面计算（纯函数直接测）。
use nd_view_model::{ComposerInput, composer_input, conversation};
use nd_wire::{Fallback, Item, Snapshot};
use serde_json::{Value, json};

fn item(id: &str, kind: &str, data: Value) -> Item {
    Item {
        id: id.into(),
        namespace: "session".into(),
        kind: kind.into(),
        data,
        fallback: Fallback {
            title: format!("{kind} 后备"),
            text: format!("{id} 后备文字"),
        },
    }
}
fn snapshot(items: Vec<Item>) -> Snapshot {
    Snapshot {
        stream: "session/s".into(),
        epoch: "e".into(),
        cursor: 1,
        items,
    }
}
fn header(process: Value, degraded: Value) -> Item {
    item(
        "header",
        "header",
        json!({"seq":1,"status":"active","cwd":"/p","model":"m","process":process,"degraded":degraded}),
    )
}
fn features(off: &[&str]) -> Value {
    json!(
        ["summarize", "bang_mode", "fork_subagent"]
            .iter()
            .map(|id| json!({"id": id, "label": id, "available": !off.contains(id)}))
            .collect::<Vec<_>>()
    )
}

#[test]
fn composer_text_starting_with_bang_or_subtask_is_an_invocation() {
    assert_eq!(
        composer_input("!ls -la"),
        ComposerInput::Shell("ls -la".into())
    );
    assert_eq!(
        composer_input("!  pwd  "),
        ComposerInput::Shell("pwd".into())
    );
    assert_eq!(
        composer_input("/subtask 查一下依赖\n第二行"),
        ComposerInput::Subtask("查一下依赖\n第二行".into())
    );
    assert_eq!(composer_input("你好 !ls"), ComposerInput::Message);
    assert_eq!(composer_input("/subtasks 不是命令"), ComposerInput::Message);
    assert!(matches!(composer_input("!"), ComposerInput::Empty(_)));
    assert!(matches!(
        composer_input("/subtask   "),
        ComposerInput::Empty(_)
    ));
}

#[test]
fn a_degraded_header_names_the_reason_and_every_unavailable_feature() {
    let view = conversation(&snapshot(vec![header(
        json!({"turn_running":false,"features":features(&["summarize","bang_mode","fork_subagent"])}),
        json!({"why":"new-desktop 没有在 4000 毫秒内报到","unavailable":["退役","总结","! 模式","fork 型子代理"]}),
    )]));
    let degraded = view.degraded.expect("degraded notice");
    assert!(
        degraded.contains("new-desktop 没有在 4000 毫秒内报到"),
        "{degraded}"
    );
    for name in ["退役", "总结", "! 模式", "fork 型子代理"] {
        assert!(degraded.contains(name), "{name}: {degraded}");
    }
    assert!(!view.abilities.shell && !view.abilities.subtask && !view.abilities.summarize);
    let healthy = conversation(&snapshot(vec![header(
        json!({"turn_running":false,"features":features(&[])}),
        Value::Null,
    )]));
    assert!(healthy.degraded.is_none());
    assert!(healthy.abilities.shell && healthy.abilities.subtask && healthy.abilities.summarize);
}

#[test]
fn summarize_is_offered_only_on_prompts_still_in_the_conversation() {
    let prompt = |id: &str, seq: u64| {
        item(
            &format!("prompt/{id}"),
            "prompt",
            json!({"seq":seq,"message":id,"text":id,"state":"landed"}),
        )
    };
    let lineage = item(
        "lineage",
        "lineage",
        json!({"seq":2,"rounds":[{"messages":["m1"]},{"messages":["m2"]}],"summarized":["m1"]}),
    );
    let items = vec![
        header(
            json!({"turn_running":false,"features":features(&[])}),
            Value::Null,
        ),
        lineage.clone(),
        prompt("m1", 3),
        prompt("m2", 4),
        prompt("m3", 5),
    ];
    let view = conversation(&snapshot(items.clone()));
    let offered: Vec<(&str, bool)> = view
        .messages
        .iter()
        .filter(|m| m.kind == "prompt")
        .map(|m| (m.id.as_str(), m.summarize.is_some()))
        .collect();
    // m1 已被总结；m3 还没进当前段的轮（没送达）。
    assert_eq!(
        offered,
        [
            ("prompt/m1", false),
            ("prompt/m2", true),
            ("prompt/m3", false)
        ]
    );
    assert_eq!(
        view.messages
            .iter()
            .find(|m| m.id == "prompt/m2")
            .unwrap()
            .summarize
            .as_deref(),
        Some("m2")
    );
    let mut degraded = items;
    degraded[0] = header(
        json!({"turn_running":false,"features":features(&["summarize"])}),
        json!({"why":"x","unavailable":["总结"]}),
    );
    assert!(
        conversation(&snapshot(degraded))
            .messages
            .iter()
            .all(|m| m.summarize.is_none())
    );
}

#[test]
fn invocation_items_show_what_ran_and_how_it_ended() {
    let view = conversation(&snapshot(vec![
        header(
            json!({"turn_running":false,"features":features(&[])}),
            Value::Null,
        ),
        item(
            "invoke/b1",
            "shell",
            json!({"seq":2,"command":"pwd","state":"done","exit":3,"stdout":"/p/sub\n","stderr":"oops"}),
        ),
        item(
            "invoke/c1",
            "compact",
            json!({"seq":3,"scope":"from","message":"m1","state":"rejected","reason":"对话里找不到这条提示"}),
        ),
        item(
            "invoke/f1",
            "subtask",
            json!({"seq":4,"prompt":"查资料","state":"done","agent":"a1b2"}),
        ),
    ]));
    let shell = view.messages.iter().find(|m| m.id == "invoke/b1").unwrap();
    assert!(shell.text.contains("! pwd"), "{}", shell.text);
    assert!(
        shell.text.contains("/p/sub") && shell.text.contains("oops"),
        "{}",
        shell.text
    );
    assert!(shell.status.contains('3'), "{}", shell.status);
    let compact = view.messages.iter().find(|m| m.id == "invoke/c1").unwrap();
    assert!(compact.text.contains("从这里总结"), "{}", compact.text);
    assert_eq!(compact.status, "没有执行");
    assert_eq!(compact.detail, "对话里找不到这条提示");
    let fork = view.messages.iter().find(|m| m.id == "invoke/f1").unwrap();
    assert!(
        fork.text.contains("查资料") && fork.text.contains("a1b2"),
        "{}",
        fork.text
    );
    assert_eq!(fork.status, "已派出");
}
