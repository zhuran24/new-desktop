use nd_view_model::session_settings;
use nd_wire::{Fallback, Item, Snapshot};
use serde_json::json;

#[test]
fn settings_controls_follow_capabilities_and_the_backend_model_catalog() {
    let mut snapshot = Snapshot {
        stream: "session/s".into(),
        epoch: "e".into(),
        cursor: 1,
        items: vec![Item {
            id: "header".into(),
            namespace: "session".into(),
            kind: "header".into(),
            fallback: Fallback {
                title: "会话".into(),
                text: "".into(),
            },
            data: json!({"session":"s","status":"active","backend":"claude","model":"opus","title":"标题","caps":{"model":true,"effort":true,"permission_mode":true,"ultracode":true},"settings":{"applied":{"model":"claude-opus-5-5","effort":"high","ultracode":true,"ultracode_requested":true},"models":[{"value":"opus","label":"Opus","resolved_model":"claude-opus-5-5","effort_levels":["low","high","xhigh"]}]}}),
        }],
    };
    let view = session_settings(&snapshot);
    assert_eq!(view.ultracode, Some(true));
    assert_eq!(view.models[0].value, "opus");
    assert_eq!(view.efforts, vec!["low", "high", "xhigh"]);
    assert_eq!(view.title, "标题");
    for backend in ["claude", "codex"] {
        snapshot.items[0].data["backend"] = json!(backend);
        snapshot.items[0].data["caps"]["ultracode"] = json!(false);
        assert_eq!(session_settings(&snapshot).ultracode, None);
    }
    snapshot.items[0].data["caps"] = json!({});
    assert!(session_settings(&snapshot).models.is_empty());
}
