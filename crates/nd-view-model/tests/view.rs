use nd_view_model::{ViewState, ViewStateFile};

#[test]
fn device_view_preferences_survive_reopen_without_persisting_daemon_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ui.json");
    let file = ViewStateFile::open(&path).unwrap();
    let mut state = ViewState::default();
    state.window.width = 1200.;
    state.sidebar_width = 280.;
    state.selected_session = Some("session-a".into());
    state
        .scroll_anchors
        .insert("session-a".into(), "item-17".into());
    state
        .tree_views
        .insert("tree-a".into(), "chronological".into());
    file.save(&state).unwrap();
    assert!(ViewStateFile::open(&path).is_err(), "one writer per device");
    drop(file);
    let reopened = ViewStateFile::open(&path).unwrap();
    assert_eq!(reopened.load().unwrap(), state);
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(json.get("snapshot").is_none());
    assert!(json.get("cursor").is_none());
}

#[test]
fn invalid_view_file_is_reported_and_invalid_geometry_cannot_be_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ui.json");
    let file = ViewStateFile::open(&path).unwrap();
    std::fs::write(&path, "broken").unwrap();
    assert!(file.load().is_err());
    for bad in [f32::NAN, -1., 0., 100_000.] {
        let mut state = ViewState::default();
        state.window.width = bad;
        assert!(file.save(&state).is_err());
    }
    std::fs::write(&path, r#"{"window":{"width":-1,"height":760}}"#).unwrap();
    assert!(file.load().is_err());
}

#[test]
fn optional_slots_unmount_synchronously_and_can_be_registered_again() {
    use nd_view_model::{Slot, Slots};
    let mut slots = Slots::<String>::default();
    let sidebar = slots
        .register(Slot::Sidebar, "sessions", 10, "会话".into())
        .unwrap();
    let badge = slots
        .register(Slot::Header, "tasks", 0, "任务".into())
        .unwrap();
    let renderer = slots
        .register(Slot::Item("future".into()), "future", 0, "未来条目".into())
        .unwrap();
    assert!(
        slots
            .register(Slot::Sidebar, "sessions", 5, "duplicate".into())
            .is_err()
    );
    assert_eq!(slots.values(&Slot::Sidebar), vec!["会话".to_owned()]);
    drop(sidebar);
    assert!(slots.values(&Slot::Sidebar).is_empty());
    assert_eq!(slots.values(&Slot::Header), vec!["任务".to_owned()]);
    let _new = slots
        .register(Slot::Sidebar, "sessions", 0, "会话".into())
        .unwrap();
    drop(badge);
    drop(renderer);
    assert!(slots.values(&Slot::Item("future".into())).is_empty());
}

#[test]
fn unknown_items_have_readable_fallback_and_theme_changes_leave_facts_untouched() {
    use nd_view_model::{Theme, ThemeMode, project};
    let snapshot = nd_wire::Snapshot {
        stream: "global".into(),
        epoch: "e".into(),
        cursor: 0,
        items: vec![nd_wire::Item {
            id: "one".into(),
            namespace: "future".into(),
            kind: "v99".into(),
            data: serde_json::json!({"new": true}),
            fallback: nd_wire::Fallback {
                title: "未来功能".into(),
                text: "仍然能读到这段文字".into(),
            },
        }],
    };
    let state = ViewState::default();
    let light = Theme::builtin(ThemeMode::Light);
    let dark = Theme::builtin(ThemeMode::Dark);
    let view = project(&snapshot, &state);
    assert_eq!(view.items[0].title, "未来功能");
    assert_eq!(view.items[0].text, "仍然能读到这段文字");
    assert_ne!(light.colors.background, dark.colors.background);
    assert_ne!(light.colors.foreground, dark.colors.foreground);
    let mut selected = state;
    selected.selected_session = Some("one".into());
    assert!(project(&snapshot, &selected).items[0].selected);
    assert!(!view.items[0].selected);
}

#[test]
fn configuration_unloads_every_slot_of_one_component_and_preserves_other_components() {
    use nd_view_model::{Contribution, Slot, Slots};
    let mut slots = Slots::<String>::default();
    let entries = vec![
        Contribution {
            slot: Slot::Sidebar,
            order: 10,
            value: "会话".into(),
        },
        Contribution {
            slot: Slot::Header,
            order: 0,
            value: "状态".into(),
        },
    ];
    slots.configure("overview", &entries, true).unwrap();
    let _other = slots
        .register(Slot::Header, "other", 20, "另一个".into())
        .unwrap();
    assert_eq!(
        slots.values(&Slot::Header),
        vec!["状态".to_owned(), "另一个".to_owned()]
    );
    slots.configure("overview", &entries, false).unwrap();
    assert!(slots.values(&Slot::Sidebar).is_empty());
    assert_eq!(slots.values(&Slot::Header), vec!["另一个".to_owned()]);
    slots.configure("overview", &entries, true).unwrap();
    assert_eq!(slots.values(&Slot::Sidebar), vec!["会话".to_owned()]);
}
