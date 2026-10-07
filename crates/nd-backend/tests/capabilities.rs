use nd_backend::SettingCaps;
use nd_backend::{BackendKind, session_capabilities};

#[test]
fn ultracode_is_denied_for_codex_and_missing_capability_reports() {
    assert_eq!(
        session_capabilities(
            &BackendKind::Codex,
            &SettingCaps {
                ultracode: true,
                ..Default::default()
            }
        )
        .ultracode,
        false
    );
    assert_eq!(
        session_capabilities(&BackendKind::Claude, &SettingCaps::default()).ultracode,
        false
    );
    assert_eq!(
        session_capabilities(
            &BackendKind::Claude,
            &SettingCaps {
                ultracode: true,
                ..Default::default()
            }
        )
        .ultracode,
        true
    );
}
