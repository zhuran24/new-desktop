use nd_backend::{BackendKind, session_capabilities};
use serde_json::json;

#[test]
fn ultracode_is_denied_for_codex_and_missing_capability_reports() {
    assert_eq!(
        session_capabilities(&BackendKind::Codex, &json!({"ultracode":true}))["ultracode"],
        false
    );
    assert_eq!(
        session_capabilities(&BackendKind::Claude, &json!({}))["ultracode"],
        false
    );
    assert_eq!(
        session_capabilities(&BackendKind::Claude, &json!({"ultracode":true}))["ultracode"],
        true
    );
}
