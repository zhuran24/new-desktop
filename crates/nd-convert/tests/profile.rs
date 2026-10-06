use nd_convert::{BackendKind, NativeProfile, ProfileTarget, decode_profile, encode_profile};
use serde_json::json;

#[test]
fn settings_map_through_neutral_profile_with_an_explicit_target_model() {
    let source = NativeProfile {
        backend: BackendKind::Claude,
        model: "claude-source".into(),
        cwd: "/project/中文".into(),
        settings: json!({"permissionMode":"acceptEdits","effort":"high","startup":{"plugin":"source-only"}}),
    };
    let neutral = decode_profile(&source).unwrap();
    let target = ProfileTarget {
        backend: BackendKind::Codex,
        model: "gpt-selected".into(),
        defaults: json!({"approvalPolicy":"untrusted","sandbox":"read-only","effort":"medium"}),
        supported_efforts: vec!["medium".into(), "high".into()],
    };
    let mapped = encode_profile(&neutral, &target).unwrap();
    assert_eq!(mapped.profile.model, "gpt-selected");
    assert_eq!(mapped.profile.cwd, "/project/中文");
    assert_eq!(
        mapped.profile.settings,
        json!({"approvalPolicy":"on-request","sandbox":"workspace-write","effort":"high"})
    );
    assert!(
        mapped
            .loss
            .entries
            .iter()
            .any(|l| l.position == "profile/startup")
    );
    let original = encode_profile(
        &neutral,
        &ProfileTarget {
            backend: BackendKind::Claude,
            model: source.model.clone(),
            defaults: json!({}),
            supported_efforts: vec!["high".into()],
        },
    )
    .unwrap();
    assert_eq!(original.profile, source);
}

#[test]
fn neutral_permissions_can_change_and_unknown_settings_use_target_defaults() {
    let source = NativeProfile {
        backend: BackendKind::Codex,
        model: "gpt".into(),
        cwd: "/work".into(),
        settings: json!({"approvalPolicy":"never","sandbox":"danger-full-access","effort":"future-effort","startup":{"native":1}}),
    };
    let mut neutral = decode_profile(&source).unwrap();
    neutral.permission = nd_convert::Permission::ReadOnly;
    let target = ProfileTarget {
        backend: BackendKind::Codex,
        model: "chosen".into(),
        defaults: json!({"approvalPolicy":"untrusted","sandbox":"read-only","effort":"low"}),
        supported_efforts: vec!["low".into()],
    };
    let same = encode_profile(&neutral, &target).unwrap();
    assert_eq!(same.profile.settings["sandbox"], "read-only");
    assert_eq!(same.profile.settings["approvalPolicy"], "on-request");
    assert_eq!(same.profile.settings["effort"], "low");
    let source = NativeProfile {
        settings: json!({"approvalPolicy":{"granular":{"rules":false}},"sandbox":"workspace-write","effort":"xhigh"}),
        ..source
    };
    let cross = encode_profile(
        &decode_profile(&source).unwrap(),
        &ProfileTarget {
            backend: BackendKind::Claude,
            model: "claude-chosen".into(),
            defaults: json!({"permissionMode":"default","effort":"medium"}),
            supported_efforts: vec!["medium".into()],
        },
    )
    .unwrap();
    assert_eq!(
        cross.profile.settings,
        json!({"permissionMode":"default","effort":"medium"})
    );
    assert_eq!(cross.loss.entries.len(), 2);
}

#[test]
fn resetting_neutral_permission_removes_the_previous_unrestricted_mode() {
    let source = NativeProfile {
        backend: BackendKind::Claude,
        model: "source".into(),
        cwd: "/work".into(),
        settings: json!({"permissionMode":"bypassPermissions"}),
    };
    let mut neutral = decode_profile(&source).unwrap();
    neutral.permission = nd_convert::Permission::Default;
    neutral.effort = Some(nd_convert::Effort::Maximum);
    let mapped = encode_profile(
        &neutral,
        &ProfileTarget {
            backend: BackendKind::Claude,
            model: "chosen".into(),
            defaults: json!({"permissionMode":"default","effort":"medium"}),
            supported_efforts: vec!["medium".into()],
        },
    )
    .unwrap();
    assert_eq!(
        mapped.profile.settings,
        json!({"permissionMode":"default","effort":"medium"})
    );
    assert_eq!(
        mapped.loss.entries[0].reason,
        "unsupported_effort_defaulted"
    );
}
