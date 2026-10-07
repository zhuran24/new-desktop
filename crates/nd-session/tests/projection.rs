//! 对话投影的差分基准：任意一串要显示的事，最简版（每次从头扫）与增量版结果一样。
use nd_backend::{Item, ItemKind};
use nd_session::projection::{Projection, Shown, project};
use proptest::prelude::*;
use serde_json::json;

fn shown() -> impl Strategy<Value = Shown> {
    let id = 0u8..4;
    prop_oneof![
        (id.clone(), "[a-z]{0,3}").prop_map(|(i, text)| Shown::Delta {
            item: format!("m:{i}"),
            kind: ItemKind::Text,
            text,
        }),
        (id.clone(), "[a-z]{0,6}").prop_map(|(i, text)| Shown::Block {
            item: Item {
                id: format!("m:{i}"),
                kind: ItemKind::Text,
                text,
                raw: json!(null),
            },
        }),
        (
            id.clone(),
            prop_oneof![Just("held"), Just("written"), Just("landed")]
        )
            .prop_map(|(i, state)| Shown::Prompt {
                attachments: vec![],
                id: format!("p{i}"),
                text: "hi".into(),
                intent: "fold".into(),
                state: state.into(),
                native: None,
                reason: None,
            }),
        (0u64..3, any::<bool>()).prop_map(|(n, ok)| Shown::Turn {
            carrier: "c".into(),
            n,
            ok,
            subtype: "success".into(),
            error: None,
        }),
        prop_oneof![Just("preparing"), Just("active")].prop_map(|status| Shown::Header {
            data: json!({"status": status, "cwd": "/p"}),
        }),
    ]
}

proptest! {
    #[test]
    fn simple_and_incremental_projections_agree(log in prop::collection::vec(shown(), 0..40)) {
        let mut incremental = Projection::default();
        for (cut, item) in log.iter().enumerate() {
            incremental.apply(item);
            prop_assert_eq!(project(&log[..=cut]), incremental.items());
        }
    }
}

#[test]
fn shell_fallback_reports_that_output_did_not_enter_the_conversation() {
    let items = project(&[Shown::Invoke {
        id: "shell".into(),
        kind: "shell".into(),
        data: json!({"state":"done","command":"pwd","exit":0,"stdout":"/p","appended":false}),
    }]);
    assert!(items[0].fallback.text.contains("输出没有进对话"));
}
